use eframe::egui;
use polyorama_ui_egui::{
    SemanticUiId, TextAuditCoverage, UiNode, UiRect, UiRole, UiSnapshot, audit_text_layouts,
    text_audit_coverage,
};
use serde::Serialize;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct VirtualisationObservation {
    pub total_rows: usize,
    pub visible_rows: (usize, usize),
    pub materialised_rows: (usize, usize),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct InteractionObservation {
    pub selected_obligation: Option<String>,
    pub active_pane: u32,
    pub connection: String,
    pub status: String,
    pub snapshot_busy: bool,
    pub topic_busy: bool,
    pub action_busy: bool,
    pub confirmation_action: Option<String>,
    pub confirmation_obligation: Option<String>,
    pub confirmation_occurrence: Option<u32>,
    pub confirmation_consequence: Option<String>,
    pub confirmation_fingerprint: Option<String>,
    pub confirmation_prompt: Option<String>,
    pub confirmation_conflict: Option<String>,
}

/// A pass-local explanation for painting outside the measured component recipes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RawPresentationObservation {
    pub id: SemanticUiId,
    pub reason: String,
}

impl From<polyorama_ui_egui::RawPresentation> for RawPresentationObservation {
    fn from(value: polyorama_ui_egui::RawPresentation) -> Self {
        Self {
            id: value.id,
            reason: value.reason.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TestSnapshot {
    /// Application render-pass sequence, including discarded egui layout passes.
    pub frame_number: u64,
    pub ui_snapshot: UiSnapshot,
    /// Includes clipped raw attempts; these are annotations, not text measurements.
    pub raw_presentations: Vec<RawPresentationObservation>,
    pub virtualisation: VirtualisationObservation,
    /// Model state used to render these nodes, before this pass's intents apply.
    pub interaction: InteractionObservation,
}

pub fn root_node(rect: egui::Rect) -> UiNode {
    UiNode::container(
        SemanticUiId::root(),
        None,
        UiRole::Application,
        UiRect::from(rect),
    )
}

pub fn finish_snapshot(
    context: &egui::Context,
    frame_number: u64,
    mut nodes: Vec<UiNode>,
    mut text: Vec<polyorama_ui_egui::TextLayoutObservation>,
    raw_presentations: Vec<RawPresentationObservation>,
    virtualisation: VirtualisationObservation,
    interaction: InteractionObservation,
) -> TestSnapshot {
    // A malformed zero-sized node is never useful to physical automation and
    // would make a transient clipped widget poison the complete current frame.
    let root_rect = nodes.first().map(|node| node.rect).unwrap_or_default();
    nodes.retain(|node| node.rect.is_positive() && root_rect.contains(node.rect, 1.0));
    // Overscanned rows are laid out beyond a scroll area's clip. Keep only
    // complete current-frame text observations; partially or wholly clipped
    // rows remain represented by the explicit virtualisation range.
    text.retain(|item| {
        if item.layout_error.is_some() {
            return true;
        }
        let allocation = item.allocated_rect;
        let clip = item.clip_rect;
        allocation.max_x > allocation.min_x
            && allocation.max_y > allocation.min_y
            && clip.max_x > clip.min_x
            && clip.max_y > clip.min_y
            && allocation.min_x >= clip.min_x - 1.0
            && allocation.min_y >= clip.min_y - 1.0
            && allocation.max_x <= clip.max_x + 1.0
            && allocation.max_y <= clip.max_y + 1.0
    });
    let text_audit = audit_text_layouts(&text);
    let coverage: TextAuditCoverage = text_audit_coverage(context, &text);
    let mut ui_snapshot = UiSnapshot {
        frame: frame_number,
        pixels_per_point: context.pixels_per_point(),
        root: SemanticUiId::root(),
        nodes,
        text,
        text_audit,
        text_audit_coverage: Some(coverage),
        semantic_audit: Vec::new(),
    };
    ui_snapshot.semantic_audit = ui_snapshot.audit();
    TestSnapshot {
        frame_number,
        ui_snapshot,
        raw_presentations,
        virtualisation,
        interaction,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_text_attempts_survive_offscreen_filtering() {
        use polyorama_ui_egui::{
            TextInteraction, TextOverflow, TextRole, UiPreferences, measured_content_label,
        };
        let context = egui::Context::default();
        let mut observations = Vec::new();
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            measured_content_label(
                ui,
                99,
                "invalid evidence request",
                TextRole::Body,
                TextOverflow::Wrap,
                24,
                TextInteraction::Inert,
                &UiPreferences::default().tokens(false),
                1.0,
                &mut observations,
            );
        });
        output.textures_delta.clear();
        assert_eq!(observations.len(), 1);
        observations[0].clip_rect = egui::Rect::NOTHING.into();
        let snapshot = finish_snapshot(
            &context,
            1,
            Vec::new(),
            observations,
            Vec::new(),
            VirtualisationObservation::default(),
            InteractionObservation::default(),
        );
        assert_eq!(snapshot.ui_snapshot.text.len(), 1);
        assert!(!snapshot.ui_snapshot.text_audit.is_empty());
    }

    #[test]
    fn current_frame_snapshot_excludes_offscreen_nodes_and_audits_cleanly() {
        let context = egui::Context::default();
        let root = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let mut offscreen = UiNode::container(
            SemanticUiId::new("offscreen"),
            Some(SemanticUiId::root()),
            UiRole::ResultRow,
            egui::Rect::from_min_size(egui::pos2(0.0, 700.0), egui::vec2(100.0, 40.0)).into(),
        );
        offscreen.name = "Overscanned row".to_owned();
        let snapshot = finish_snapshot(
            &context,
            7,
            vec![root_node(root), offscreen],
            Vec::new(),
            vec![RawPresentationObservation {
                id: SemanticUiId::new("offscreen.raw"),
                reason: "Overscanned custom row painter".to_owned(),
            }],
            VirtualisationObservation {
                total_rows: 50_000,
                visible_rows: (120, 126),
                materialised_rows: (116, 130),
            },
            InteractionObservation::default(),
        );
        assert_eq!(snapshot.ui_snapshot.nodes.len(), 1);
        assert_eq!(snapshot.raw_presentations.len(), 1);
        assert_eq!(
            snapshot.raw_presentations[0].id,
            SemanticUiId::new("offscreen.raw")
        );
        assert!(snapshot.ui_snapshot.semantic_audit.is_empty());
        assert_eq!(snapshot.virtualisation.materialised_rows, (116, 130));
    }
}
