use super::*;
use serde::Deserialize;

#[derive(Default, Deserialize)]
#[serde(default)]
struct BrowserPushState {
    busy: bool,
    error: String,
    status: String,
    supported: bool,
    installed: bool,
    ios: bool,
    permission: String,
    configured: bool,
    configuration_revision: Option<i64>,
    ready: bool,
    active: bool,
    device_label: String,
    local_device: bool,
    can_enable: bool,
    can_disable: bool,
    pending: bool,
    disable_review: Option<String>,
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(inline_js = "
export function push_state() { return window.__BOKKIE_PUSH?.snapshotJSON() ?? '{}'; }
export function push_action(action, label) { window.__BOKKIE_PUSH?.[action](label); }
export function push_task_link() { return window.__BOKKIE_PUSH?.takeTaskLink() ?? null; }
")]
extern "C" {
    fn push_state() -> String;
    fn push_action(action: &str, label: &str);
    fn push_task_link() -> Option<String>;
}

fn browser_state() -> BrowserPushState {
    #[cfg(target_arch = "wasm32")]
    {
        serde_json::from_str(&push_state()).unwrap_or_default()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        BrowserPushState::default()
    }
}

fn action(name: &str, label: &str) {
    #[cfg(target_arch = "wasm32")]
    push_action(name, label);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (name, label);
}

pub(super) fn take_task_link() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        push_task_link()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
}

pub(super) fn configuration_revision() -> Option<i64> {
    browser_state().configuration_revision
}

pub(super) fn refresh() {
    action("refresh", "");
}

pub(super) fn show(ui: &mut egui::Ui, label: &mut String, safe: bool, nodes: &mut Vec<UiNode>) {
    let state = browser_state();
    show_state(ui, label, safe, nodes, &state, cfg!(target_arch = "wasm32"));
}

fn show_state(
    ui: &mut egui::Ui,
    label: &mut String,
    safe: bool,
    nodes: &mut Vec<UiNode>,
    state: &BrowserPushState,
    browser: bool,
) {
    use conversation_ui::button;
    ui.add(
        egui::Label::new(
            "Receive Bokkie reminders as system notifications on one device you choose.",
        )
        .wrap(),
    );
    ui.add_space(8.0);
    if !browser {
        ui.add(egui::Label::new("Open Bokkie in a supported browser and install it there to set up notifications. This native attention interface does not receive Web Push.").wrap());
        return;
    }
    ui.add(egui::Label::new(&state.status).wrap());
    if !state.error.is_empty() {
        ui.add(egui::Label::new(&state.error).wrap());
    }
    if !state.configured {
        ui.add(egui::Label::new("Push notifications are unavailable in this Bokkie service. Configure the notification transport before enabling a device.").wrap());
    }
    if state.ios && !state.installed {
        ui.add(egui::Label::new("On iPhone or iPad, use Share → Add to Home Screen, then open Bokkie from its icon. Enable notifications inside that installed app.").wrap());
    } else if !state.supported {
        ui.add(egui::Label::new("This browser cannot receive Web Push here. Use a supported browser with a secure Bokkie address.").wrap());
    } else if !state.installed {
        ui.add(egui::Label::new("Install Bokkie using your browser's Install app or Add to Home Screen control. Notifications belong to the browser or installed app you enable here.").wrap());
    }
    if state.permission == "denied" {
        ui.add(egui::Label::new("Notification permission is blocked. Allow Bokkie notifications in browser or device settings, then refresh. Bokkie cannot change that permission for you.").wrap());
    }
    if state.active {
        ui.add(egui::Label::new(format!("Configured device: {}", state.device_label)).wrap());
        ui.add(egui::Label::new(if state.local_device {
            "This browser was enrolled as that device. Check its notification permission and device settings if alerts stop."
        } else {
            "A device is already enrolled. Disable the reviewed device before choosing another; Bokkie will not replace it silently."
        }).wrap());
    }
    if state.can_enable || state.pending {
        ui.label("Device name");
        let response = ui.add_enabled(
            safe && !state.busy && !state.pending,
            egui::TextEdit::singleline(label)
                .id_salt("bokkie.notifications.device-label")
                .desired_width(ui.available_width()),
        );
        conversation_ui::observe(
            response.rect,
            "bokkie.notifications.device-label",
            "Notification device name",
            UiRole::Section,
            safe && !state.busy && !state.pending,
            nodes,
        );
        if button(
            ui,
            "bokkie.notifications.enable",
            if state.pending {
                "Retry exact device enrolment"
            } else {
                "Enable notifications on this device"
            },
            safe && state.can_enable
                && (state.pending || (1..=80).contains(&label.trim().chars().count())),
            nodes,
        ) {
            // Direct call preserves browser user activation for requestPermission.
            action("enable", label);
        }
    }
    if let Some(device) = &state.disable_review {
        ui.separator();
        ui.add(egui::Label::new(format!("Disable {device} for future reminders? Already admitted reminders keep their original destination, and history remains available.")).wrap());
        if button(
            ui,
            "bokkie.notifications.disable-confirm",
            "Confirm disable device",
            safe && !state.busy,
            nodes,
        ) {
            action("disable", "");
        }
        if button(
            ui,
            "bokkie.notifications.disable-cancel",
            "Keep device",
            !state.busy,
            nodes,
        ) {
            action("cancelDisable", "");
        }
    } else if state.can_disable
        && button(
            ui,
            "bokkie.notifications.disable-review",
            "Review disabling device",
            safe,
            nodes,
        )
    {
        action("reviewDisable", "");
    }
    if button(
        ui,
        "bokkie.notifications.refresh",
        "Refresh notification settings",
        !state.busy,
        nodes,
    ) {
        refresh();
    }
    if state.busy || (!state.ready && state.configured) {
        ui.ctx().request_repaint_after(Duration::from_secs(1));
    }
    ui.add_space(10.0);
    ui.add(egui::Label::new("A push service accepting a reminder does not prove an alert appeared. Device display or opening reports may arrive later, including after Bokkie reconnects. Check device notification settings when no report is available; reminders are not automatically resent for missing device reports.").wrap());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_acceptance_and_device_evidence_remain_distinct() {
        let mut delivery = bokkie_operator_api::ManagedDelivery {
            id: "delivery".into(),
            status: "accepted_by_push_service".into(),
            detail: "Accepted".into(),
            destination: "Bokkie".into(),
            subject: "Reminder".into(),
            body: "Review priorities".into(),
            next_retry_at: None,
            attempts: vec![],
            recovery: None,
            push: Some(bokkie_operator_api::ManagedPushDelivery {
                subscription_label: "Chosen phone".into(),
                expires_at: 1791268200,
                device_status: Some("opened".into()),
                device_reported_at: Some(1791268000),
                accepted_ttl_seconds: Some(3600),
            }),
        };
        assert!(notification_label(&delivery).contains("Accepted by the push service"));
        let evidence = notification_device_evidence(&delivery, "Australia/Adelaide").unwrap();
        assert!(evidence.contains("reported opening"));
        assert!(evidence.contains("Display deadline"));
        delivery.push.as_mut().unwrap().device_status = None;
        assert!(
            notification_device_evidence(&delivery, "Australia/Adelaide")
                .unwrap()
                .contains("does not prove that the alert was missed")
        );
        assert!(notification_label(&delivery).contains("Accepted by the push service"));
    }

    #[test]
    fn unsupported_and_native_surfaces_do_not_offer_enrolment() {
        for browser in [true, false] {
            let context = egui::Context::default();
            let mut nodes = vec![];
            let mut label = "Phone".to_owned();
            context
                .run_ui(egui::RawInput::default(), |ui| {
                    show_state(
                        ui,
                        &mut label,
                        true,
                        &mut nodes,
                        &BrowserPushState::default(),
                        browser,
                    );
                })
                .textures_delta
                .clear();
            assert!(
                !nodes
                    .iter()
                    .any(|node| node.id.0 == "bokkie.notifications.enable")
            );
        }
    }

    #[test]
    fn device_disable_has_separate_confirmation_and_stale_state_disables_actions() {
        let state = BrowserPushState {
            configured: true,
            supported: true,
            ready: true,
            can_disable: true,
            disable_review: Some("Reviewed phone".into()),
            ..Default::default()
        };
        let context = egui::Context::default();
        let mut nodes = vec![];
        let mut label = "Phone".to_owned();
        context
            .run_ui(egui::RawInput::default(), |ui| {
                show_state(ui, &mut label, false, &mut nodes, &state, true);
            })
            .textures_delta
            .clear();
        assert!(
            nodes
                .iter()
                .any(|node| node.id.0 == "bokkie.notifications.disable-confirm" && !node.enabled)
        );
        assert!(
            !nodes
                .iter()
                .any(|node| node.id.0 == "bokkie.notifications.disable-review")
        );
    }
}
