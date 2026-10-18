//! The event WebSocket to `GET /api/v1/events`, at the page's own origin.
//!
//! It performs the I/O a [`Driver`] asks for and sends the driver's updates
//! to the application as intents, tagged with the sign-in they belong to.
//! The policy (resume, back-off, ping, reload) is the driver's, so this file
//! only wires browser callbacks to it, as Stobie's Live Feed example does:
//! one set of callbacks, created once and holding weak references, because a
//! callback may reconnect and must not drop itself.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use bokkie_protocol::EVENTS_PATH;
use stobie_ui_egui::IntentSender;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{CloseEvent, MessageEvent, VisibilityState, WebSocket};

use crate::BokkieIntent;
use crate::api::Generation;
use crate::connection::Input;
use crate::socket::{Driver, Io, Step};

/// The page's event socket, idle until started.
pub(super) struct EventSocket {
    inner: Rc<Inner>,
}

struct Inner {
    intents: IntentSender<BokkieIntent>,
    /// The sign-in this socket serves.
    generation: Cell<Generation>,
    driver: RefCell<Driver>,
    socket: RefCell<Option<WebSocket>>,
    retry: Cell<Option<i32>>,
    pong: Cell<Option<i32>>,
    /// The ping the pong timer is for.
    pong_for: Cell<u32>,
    /// The host has stopped: nothing more is sent or opened.
    stopped: Cell<bool>,
    callbacks: OnceCell<Callbacks>,
}

struct Callbacks {
    open: Closure<dyn FnMut()>,
    message: Closure<dyn FnMut(MessageEvent)>,
    close: Closure<dyn FnMut(CloseEvent)>,
    retry: Closure<dyn FnMut()>,
    pong: Closure<dyn FnMut()>,
    visibility: Closure<dyn FnMut()>,
}

impl EventSocket {
    pub(super) fn new(intents: IntentSender<BokkieIntent>) -> Self {
        let inner = Rc::new(Inner {
            intents,
            generation: Cell::new(Generation::default()),
            driver: RefCell::new(Driver::new()),
            socket: RefCell::new(None),
            retry: Cell::new(None),
            pong: Cell::new(None),
            pong_for: Cell::new(0),
            stopped: Cell::new(false),
            callbacks: OnceCell::new(),
        });
        let _ = inner.callbacks.set(Callbacks::new(Rc::downgrade(&inner)));
        if let Some(document) = web_sys::window().and_then(|window| window.document()) {
            let _ = document.add_event_listener_with_callback(
                "visibilitychange",
                inner.callbacks().visibility.as_ref().unchecked_ref(),
            );
        }
        Self { inner }
    }

    /// Connect for the sign-in `generation`, replacing any connection, with
    /// a driver that has never been live.
    pub(super) fn start(&self, generation: Generation) {
        let inner = &self.inner;
        inner.close_socket();
        inner.clear_timers();
        inner.generation.set(generation);
        *inner.driver.borrow_mut() = Driver::new();
        inner.handle(Input::Start);
    }

    /// Close for good, as on signing out.
    pub(super) fn stop(&self) {
        self.inner.handle(Input::Stop);
        self.inner.clear_timers();
    }
}

impl Drop for EventSocket {
    fn drop(&mut self) {
        self.inner.shut_down();
        if let Some(document) = web_sys::window().and_then(|window| window.document()) {
            let _ = document.remove_event_listener_with_callback(
                "visibilitychange",
                self.inner.callbacks().visibility.as_ref().unchecked_ref(),
            );
        }
    }
}

impl Callbacks {
    fn new(inner: Weak<Inner>) -> Self {
        let on = |inner: &Weak<Inner>, run: fn(&Inner)| {
            let inner = inner.clone();
            move || {
                if let Some(inner) = inner.upgrade() {
                    run(&inner);
                }
            }
        };
        let message = {
            let inner = inner.clone();
            move |event: MessageEvent| {
                if let Some(inner) = inner.upgrade() {
                    let step = match event.data().as_string() {
                        Some(text) => inner.driver.borrow_mut().receive(&text),
                        // The gateway sends text only.
                        None => inner.driver.borrow_mut().handle(Input::Unreadable),
                    };
                    inner.perform(step);
                }
            }
        };
        let close = {
            let inner = inner.clone();
            move |event: CloseEvent| {
                if let Some(inner) = inner.upgrade() {
                    inner.close_socket();
                    inner.handle(Input::Closed {
                        code: Some(event.code()),
                    });
                }
            }
        };
        Self {
            open: Closure::new(on(&inner, |inner| inner.handle(Input::Opened))),
            message: Closure::new(message),
            close: Closure::new(close),
            retry: Closure::new(on(&inner, |inner| {
                inner.retry.set(None);
                inner.handle(Input::RetryDue);
            })),
            pong: Closure::new(on(&inner, |inner| {
                inner.pong.set(None);
                inner.handle(Input::PongOverdue {
                    ping: inner.pong_for.get(),
                });
            })),
            visibility: Closure::new(on(&inner, |inner| {
                let visible = web_sys::window()
                    .and_then(|window| window.document())
                    .is_some_and(|document| {
                        document.visibility_state() == VisibilityState::Visible
                    });
                if visible {
                    inner.handle(Input::Visible);
                }
            })),
        }
    }
}

impl Inner {
    fn callbacks(&self) -> &Callbacks {
        self.callbacks.get().expect("callbacks are set at creation")
    }

    fn handle(&self, input: Input) {
        if self.stopped.get() {
            return;
        }
        let step = self.driver.borrow_mut().handle(input);
        self.perform(step);
    }

    fn perform(&self, step: Step) {
        for io in step.io {
            if self.stopped.get() {
                return;
            }
            match io {
                Io::Connect => self.connect(),
                Io::Send(text) => {
                    if let Some(socket) = self.socket.borrow().as_ref() {
                        // A socket that cannot send is closing and says so.
                        let _ = socket.send_with_str(&text);
                    }
                }
                Io::Close => self.close_socket(),
                Io::RetryAfter(delay) => {
                    clear(&self.retry);
                    self.retry.set(set_timeout(&self.callbacks().retry, delay));
                }
                Io::CancelRetry => clear(&self.retry),
                Io::AwaitPong { ping, after } => {
                    clear(&self.pong);
                    self.pong_for.set(ping);
                    self.pong.set(set_timeout(&self.callbacks().pong, after));
                }
            }
        }
        let generation = self.generation.get();
        for update in step.updates {
            if self
                .intents
                .send(BokkieIntent::Socket { generation, update })
                .is_err()
            {
                // The host has stopped: nobody is listening.
                self.shut_down();
                return;
            }
        }
    }

    fn connect(&self) {
        self.close_socket();
        let Some(socket) = events_url().and_then(|url| WebSocket::new(&url).ok()) else {
            // As a browser reports a socket that never opened.
            self.handle(Input::Closed { code: None });
            return;
        };
        let callbacks = self.callbacks();
        socket.set_onopen(Some(callbacks.open.as_ref().unchecked_ref()));
        socket.set_onmessage(Some(callbacks.message.as_ref().unchecked_ref()));
        // A failed connection also closes, so close covers errors.
        socket.set_onclose(Some(callbacks.close.as_ref().unchecked_ref()));
        *self.socket.borrow_mut() = Some(socket);
    }

    /// Detach and close the current socket, so none of its late callbacks
    /// arrive.
    fn close_socket(&self) {
        if let Some(socket) = self.socket.borrow_mut().take() {
            socket.set_onopen(None);
            socket.set_onmessage(None);
            socket.set_onclose(None);
            let _ = socket.close();
        }
    }

    fn clear_timers(&self) {
        clear(&self.retry);
        clear(&self.pong);
    }

    fn shut_down(&self) {
        self.stopped.set(true);
        self.close_socket();
        self.clear_timers();
    }
}

/// `ws:` or `wss:` to the event route at the page's own host.
fn events_url() -> Option<String> {
    let location = web_sys::window()?.location();
    let scheme = if location.protocol().ok()? == "https:" {
        "wss:"
    } else {
        "ws:"
    };
    Some(format!("{scheme}//{}{EVENTS_PATH}", location.host().ok()?))
}

fn set_timeout(callback: &Closure<dyn FnMut()>, delay: Duration) -> Option<i32> {
    let millis = i32::try_from(delay.as_millis()).unwrap_or(i32::MAX);
    web_sys::window()?
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            callback.as_ref().unchecked_ref(),
            millis,
        )
        .ok()
}

fn clear(timer: &Cell<Option<i32>>) {
    if let (Some(handle), Some(window)) = (timer.take(), web_sys::window()) {
        window.clear_timeout_with_handle(handle);
    }
}
