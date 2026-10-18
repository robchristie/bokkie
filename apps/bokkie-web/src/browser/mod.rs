//! The browser transport: `fetch` to the page's own origin for commands,
//! with same-origin credentials so the session cookie travels and the
//! browser adds the `Origin` header the gateway checks, and the event
//! WebSocket ([`socket`]).

mod socket;

use stobie_ui_egui::IntentSender;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestCredentials, RequestInit, Response};

use crate::BokkieIntent;
use bokkie_protocol::Timestamp;

use crate::api::{Api, ApiRequest, Generation};

/// Sends each command with `fetch` and reports its reply as an intent, and
/// runs the event socket.
pub struct BrowserApi {
    intents: IntentSender<BokkieIntent>,
    socket: socket::EventSocket,
}

impl std::fmt::Debug for BrowserApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BrowserApi")
    }
}

impl BrowserApi {
    pub fn new(intents: IntentSender<BokkieIntent>) -> Self {
        Self {
            socket: socket::EventSocket::new(intents.clone()),
            intents,
        }
    }
}

impl Api for BrowserApi {
    fn send(&self, generation: Generation, request: ApiRequest) {
        let intents = self.intents.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let reply = match fetch(&request).await {
                Ok((status, body)) => request.decode(status, &body),
                Err(_) => request.unreachable(),
            };
            // A stopped host has nobody left to tell.
            let _ = intents.send(BokkieIntent::Reply { generation, reply });
        });
    }

    fn connect_events(&self, generation: Generation) {
        self.socket.start(generation);
    }

    fn disconnect_events(&self) {
        self.socket.stop();
    }

    fn reload_page(&self) {
        if let Some(window) = web_sys::window() {
            let _ = window.location().reload();
        }
    }

    fn now(&self) -> Timestamp {
        let millis = web_sys::window()
            .and_then(|window| window.performance())
            .map_or(0.0, |clock| clock.time_origin() + clock.now());
        Timestamp(millis as i64)
    }
}

async fn fetch(request: &ApiRequest) -> Result<(u16, String), JsValue> {
    let init = RequestInit::new();
    init.set_method(request.method().as_str());
    init.set_credentials(RequestCredentials::SameOrigin);
    if let Some(body) = request.body() {
        init.set_body(&JsValue::from_str(&body));
    }
    let outgoing = Request::new_with_str_and_init(&request.path(), &init)?;
    let headers = outgoing.headers();
    headers.set("Accept", "application/json")?;
    if request.body().is_some() {
        headers.set("Content-Type", "application/json")?;
    }
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let response: Response = JsFuture::from(window.fetch_with_request(&outgoing))
        .await?
        .dyn_into()?;
    let text = JsFuture::from(response.text()?).await?;
    Ok((response.status(), text.as_string().unwrap_or_default()))
}
