mod app;
mod model;
mod transport;
mod ui_observation;

pub use app::AttentionApp;
pub use model::{
    INBOX_PANE_ID, LifecycleAction, OBLIGATIONS_PANE_ID, TIMELINE_PANE_ID, operator_workspace,
};
pub use transport::{ApiRequest, Transport};
pub use ui_observation::TestSnapshot;

pub const APPLICATION_NAME: &str = "Bokkie Operator";

#[cfg(target_arch = "wasm32")]
use serde::Serialize;
#[cfg(target_arch = "wasm32")]
use std::{cell::RefCell, rc::Rc};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
#[wasm_bindgen]
pub struct WebHandle {
    runner: eframe::WebRunner,
    observer: Rc<RefCell<TestSnapshot>>,
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
impl WebHandle {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        console_error_panic_hook::set_once();
        let _ = eframe::WebLogger::init(log::LevelFilter::Info);
        Self {
            runner: eframe::WebRunner::new(),
            observer: Rc::new(RefCell::new(TestSnapshot::default())),
        }
    }

    pub async fn start(&self, canvas: web_sys::HtmlCanvasElement) -> Result<(), JsValue> {
        let observer = self.observer.clone();
        self.runner
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(move |creation| {
                    Ok(Box::new(AttentionApp::new_observed(
                        creation,
                        Some(observer),
                    )))
                }),
            )
            .await
    }

    pub fn destroy(&self) {
        self.runner.destroy();
    }

    pub fn test_snapshot(&self) -> Result<JsValue, JsValue> {
        let serializer =
            serde_wasm_bindgen::Serializer::new().serialize_large_number_types_as_bigints(true);
        self.observer
            .borrow()
            .serialize(&serializer)
            .map_err(Into::into)
    }
}

#[cfg(target_arch = "wasm32")]
impl Default for WebHandle {
    fn default() -> Self {
        Self::new()
    }
}
