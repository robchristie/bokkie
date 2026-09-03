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

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TestSnapshot {
    pub frame_number: u64,
    pub ui_snapshot: UiSnapshot,
    pub virtualisation: VirtualisationObservation,
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
        virtualisation,
        interaction,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            VirtualisationObservation {
                total_rows: 50_000,
                visible_rows: (120, 126),
                materialised_rows: (116, 130),
            },
            InteractionObservation::default(),
        );
        assert_eq!(snapshot.ui_snapshot.nodes.len(), 1);
        assert!(snapshot.ui_snapshot.semantic_audit.is_empty());
        assert_eq!(snapshot.virtualisation.materialised_rows, (116, 130));
    }
}
