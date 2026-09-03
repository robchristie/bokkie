#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    use bokkie_attention_ui::{APPLICATION_NAME, AttentionApp};

    eframe::run_native(
        APPLICATION_NAME,
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([960.0, 640.0])
                .with_min_inner_size([640.0, 480.0]),
            ..Default::default()
        },
        Box::new(|creation| Ok(Box::new(AttentionApp::new(creation)))),
    )
}

#[cfg(target_arch = "wasm32")]
fn main() {}
