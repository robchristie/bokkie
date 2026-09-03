#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    use bokkie_attention_ui::{APPLICATION_NAME, AttentionApp};

    eframe::run_native(
        APPLICATION_NAME,
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([1280.0, 720.0])
                .with_min_inner_size([360.0, 480.0]),
            ..Default::default()
        },
        Box::new(|creation| Ok(Box::new(AttentionApp::new(creation)))),
    )
}

#[cfg(target_arch = "wasm32")]
fn main() {}
