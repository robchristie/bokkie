mod app;
mod model;
mod transport;

pub use app::AttentionApp;
pub use model::{ATTENTION_PANE_ID, AttentionIntent, fixed_workspace};
pub use transport::{ApiRequest, Transport};

pub const APPLICATION_NAME: &str = "Bokkie Attention";

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
#[wasm_bindgen]
pub struct WebHandle {
    runner: eframe::WebRunner,
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
        }
    }

    pub async fn start(&self, canvas: web_sys::HtmlCanvasElement) -> Result<(), JsValue> {
        self.runner
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|creation| Ok(Box::new(AttentionApp::new(creation)))),
            )
            .await
    }

    pub fn destroy(&self) {
        self.runner.destroy();
    }
}

#[cfg(target_arch = "wasm32")]
impl Default for WebHandle {
    fn default() -> Self {
        Self::new()
    }
}
