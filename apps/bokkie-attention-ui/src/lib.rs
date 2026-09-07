mod app;
mod appearance;
pub use appearance::Appearance;
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
    context: Rc<RefCell<Option<eframe::egui::Context>>>,
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
            context: Rc::new(RefCell::new(None)),
        }
    }

    pub async fn start(&self, canvas: web_sys::HtmlCanvasElement) -> Result<(), JsValue> {
        self.start_with_appearance(canvas, "{}").await
    }

    pub async fn start_with_appearance(
        &self,
        canvas: web_sys::HtmlCanvasElement,
        appearance: &str,
    ) -> Result<(), JsValue> {
        let appearance: Appearance = serde_json::from_str(appearance)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        let observer = self.observer.clone();
        let context = self.context.clone();
        self.runner
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(move |creation| {
                    *context.borrow_mut() = Some(creation.egui_ctx.clone());
                    Ok(Box::new(AttentionApp::new_observed_with_appearance(
                        creation,
                        Some(observer),
                        appearance,
                    )))
                }),
            )
            .await
    }

    pub fn destroy(&self) {
        self.runner.destroy();
        self.context.borrow_mut().take();
    }

    /// Request a real paint for bounded screenshot settling without changing state.
    pub fn request_repaint(&self) -> Result<(), JsValue> {
        let context = self.context.borrow();
        let context = context
            .as_ref()
            .ok_or_else(|| JsValue::from_str("Bokkie has not started"))?;
        context.request_repaint();
        Ok(())
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
