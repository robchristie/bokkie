use egui_kittest::Harness;
use stobie_ui_egui::AppHost;

use super::*;

fn harness() -> Harness<'static, AppHost<App>> {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(390.0, 844.0))
        .build_ui_state(
            |ui, host: &mut AppHost<App>| host.present(ui),
            AppHost::new(&egui::Context::default(), App::new()),
        );
    // The host must share the harness's context to be woken through it.
    let context = harness.ctx.clone();
    *harness.state_mut() = AppHost::new(&context, App::new());
    harness.run();
    harness
}

#[test]
fn the_host_presents_the_application_and_then_idles() {
    let harness = harness();
    let snapshot = harness.state().ui_snapshot();
    assert!(snapshot.by_name(APPLICATION_NAME).next().is_some());
    assert!(snapshot.by_name("Starting").next().is_some());
    assert!(
        snapshot.semantic_audit.is_empty(),
        "{:?}",
        snapshot.semantic_audit
    );
    assert!(
        !harness.ctx.has_requested_repaint(),
        "idle until something changes"
    );
}

#[test]
fn reload_is_offered_and_needs_the_browser() {
    let mut app = App::new();
    let reload = ActionTarget::application(BokkieAction::Reload);
    assert!(app.availability(reload).enabled());
    assert_eq!(
        app.perform(reload, &mut ()),
        Err("Reloading needs the browser".into())
    );
}

/// The page links the licences of the fonts the application embeds, and the
/// notices name each font. `scripts/check-web-notices` checks the texts.
#[test]
fn the_page_links_the_font_notices() {
    let page = include_str!("../web/index.html");
    assert!(page.contains(r#"<link rel="license" href="third-party-notices.txt" />"#));
    let notices = include_str!("../web/third-party-notices.txt");
    for font in [
        "Source Sans 3",
        "Hack",
        "Noto Emoji",
        "Ubuntu Light",
        "emoji-icon-font",
    ] {
        assert!(notices.contains(font), "{font}");
    }
    for licence in [
        "SIL OPEN FONT LICENSE Version 1.1",
        "UBUNTU FONT LICENCE Version 1.0",
        "MIT License",
        "BITSTREAM VERA LICENSE",
    ] {
        assert!(notices.contains(licence), "{licence}");
    }
}
