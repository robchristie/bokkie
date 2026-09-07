//! Bokkie owns its visual identity; mode, density and typography remain independent.
use eframe::egui;
use polyorama_ui_egui::{
    AppearancePreference, ApplicationGeometry, ApplicationTheme, ColourTokens, ContrastPreference,
    DesignTokens, REGULAR_FONT_FAMILY, Rgba8, SEMIBOLD_FONT_FAMILY, ThemeColours, ThemeVariant,
    TypographyProfile, UiPreferences, apply_design_system_with_theme,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Identity {
    #[default]
    Graphite,
    RestrainedBlue,
    WarmLight,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Typeface {
    #[default]
    SourceSans,
    Inter,
}

/// Bounded appearance inputs, also used by the real-application capture harness.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Appearance {
    pub identity: Identity,
    pub typeface: Typeface,
    pub light: bool,
    pub high_contrast: bool,
    pub font_scale: f32,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            identity: Identity::Graphite,
            typeface: Typeface::Inter,
            light: false,
            high_contrast: false,
            font_scale: 1.0,
        }
    }
}

impl Appearance {
    pub fn preferences(self) -> UiPreferences {
        UiPreferences {
            appearance: if self.light || self.identity == Identity::WarmLight {
                AppearancePreference::Light
            } else {
                AppearancePreference::Dark
            },
            contrast: if self.high_contrast {
                ContrastPreference::High
            } else {
                ContrastPreference::Standard
            },
            font_scale: self.font_scale,
            ..UiPreferences::default()
        }
        .validated()
    }

    pub fn theme(self) -> ApplicationTheme {
        let colours = ThemeColours {
            light: palette(self.identity, false, false),
            dark: palette(self.identity, true, false),
            light_high_contrast: palette(self.identity, false, true),
            dark_high_contrast: palette(self.identity, true, true),
        };
        ApplicationTheme::new(colours)
            .expect("Bokkie palettes satisfy the shared theme contract")
            .with_geometry(ApplicationGeometry {
                application_bar_height: 46.0,
                control_height: 34.0,
                control_radius: 6.0,
            })
            .expect("Bokkie chrome stays inside the shared geometry bounds")
    }

    pub fn tokens(self, system_dark: bool) -> DesignTokens {
        let preferences = self.preferences();
        self.theme().resolve(
            preferences.theme_variant(system_dark),
            preferences.density_variant(),
            TypographyProfile::Reading,
        )
    }

    pub fn apply(self, context: &egui::Context) {
        apply_design_system_with_theme(
            context,
            self.preferences(),
            TypographyProfile::Reading,
            &self.theme(),
        );
        if self.typeface == Typeface::Inter {
            install_inter(context);
        }
    }
}

const fn rgb(value: u32) -> Rgba8 {
    Rgba8 {
        red: (value >> 16) as u8,
        green: (value >> 8) as u8,
        blue: value as u8,
        alpha: 255,
    }
}

fn palette(identity: Identity, dark: bool, high: bool) -> ColourTokens {
    let variant = match (dark, high) {
        (true, true) => ThemeVariant::DarkHighContrast,
        (true, false) => ThemeVariant::Dark,
        (false, true) => ThemeVariant::LightHighContrast,
        (false, false) => ThemeVariant::Light,
    };
    let mut c =
        DesignTokens::resolve(variant, polyorama_ui_egui::DensityVariant::Comfortable).colours;
    if dark {
        c.surface_canvas = rgb(if high { 0x101012 } else { 0x18181b });
        c.surface_panel = rgb(if high { 0x171719 } else { 0x1d1d21 });
        c.surface_raised = rgb(0x27272c);
        c.surface_hover = rgb(0x29292f);
        c.selection_background = rgb(0x303036);
        c.text_primary = rgb(0xf0f0f2);
        c.text_muted = rgb(if high { 0xd4d4db } else { 0xaaabb4 });
        c.border_decorative = rgb(if high { 0x65656d } else { 0x333338 });
        c.border_control = rgb(if high { 0xb2b2bd } else { 0x858590 });
        c.selection_indicator = rgb(0xd4d4dc);
        c.focus_ring = rgb(if high { 0xc6ceff } else { 0xa7b4f8 });
        c.accent_primary = rgb(0xa7b4f8);
        c.accent_on_accent = rgb(0x18181b);
        c.action_primary_background = rgb(0xe4e4e7);
        c.action_primary_foreground = rgb(0x18181b);
        c.action_quiet_hover = rgb(0x303036);
        c.status_success = rgb(0xa3d4b3);
        c.status_warning = rgb(0xe5c38a);
        c.status_error = rgb(0xf0a6a6);
        if identity == Identity::RestrainedBlue {
            c.action_primary_background = rgb(0xabc4fa);
        }
    } else {
        let warm = identity == Identity::WarmLight;
        c.surface_canvas = rgb(if warm { 0xf1eee8 } else { 0xf0f0f2 });
        c.surface_panel = rgb(if warm { 0xfaf8f4 } else { 0xfafafa });
        c.surface_raised = rgb(0xffffff);
        c.surface_hover = rgb(if warm { 0xe9e4dc } else { 0xe6e6eb });
        c.selection_background = rgb(if warm { 0xe2dbd0 } else { 0xdddde4 });
        c.text_primary = rgb(0x242428);
        c.text_muted = rgb(if high { 0x414149 } else { 0x5c5c67 });
        c.border_decorative = rgb(if high { 0x80808a } else { 0xd3d3d9 });
        c.border_control = rgb(0x71717c);
        c.selection_indicator = rgb(0x51515d);
        c.focus_ring = rgb(0x4655a6);
        c.accent_primary = rgb(0x4655a6);
        c.accent_on_accent = rgb(0xffffff);
        c.action_primary_background = rgb(0x303036);
        c.action_primary_foreground = rgb(0xffffff);
        c.action_quiet_hover = c.surface_hover;
        c.status_success = rgb(0x27613d);
        c.status_warning = rgb(0x76531e);
        c.status_error = rgb(0x9e3434);
        if identity == Identity::RestrainedBlue || warm {
            c.action_primary_background = rgb(if high { 0x354b87 } else { 0x405998 });
        }
    }
    c.border_subtle = c.border_control;
    c
}

fn install_inter(context: &egui::Context) {
    for (name, bytes, family, proportional) in [
        (
            "Bokkie Inter Regular",
            include_bytes!("../assets/fonts/Inter-Regular.ttf").as_slice(),
            REGULAR_FONT_FAMILY,
            true,
        ),
        (
            "Bokkie Inter Semibold",
            include_bytes!("../assets/fonts/Inter-SemiBold.ttf").as_slice(),
            SEMIBOLD_FONT_FAMILY,
            false,
        ),
    ] {
        let mut families = vec![egui::epaint::text::InsertFontFamily {
            family: egui::FontFamily::Name(family.into()),
            priority: egui::epaint::text::FontPriority::Highest,
        }];
        if proportional {
            families.push(egui::epaint::text::InsertFontFamily {
                family: egui::FontFamily::Proportional,
                priority: egui::epaint::text::FontPriority::Highest,
            });
        }
        context.add_font(egui::epaint::text::FontInsert {
            name: name.into(),
            data: egui::FontData::from_static(bytes),
            families,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_identity_validates_all_four_modes() {
        for identity in [
            Identity::Graphite,
            Identity::RestrainedBlue,
            Identity::WarmLight,
        ] {
            let appearance = Appearance {
                identity,
                ..Appearance::default()
            };
            let _ = appearance.theme();
        }
    }
    fn contrast(a: Rgba8, b: Rgba8) -> f64 {
        let luminance = |colour: Rgba8| {
            let linear = |channel: u8| {
                let value = f64::from(channel) / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(colour.red)
                + 0.7152 * linear(colour.green)
                + 0.0722 * linear(colour.blue)
        };
        let a = luminance(a);
        let b = luminance(b);
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn collection_metadata_and_meaningful_control_edges_keep_contrast_in_every_mode() {
        for identity in [
            Identity::Graphite,
            Identity::RestrainedBlue,
            Identity::WarmLight,
        ] {
            for dark in [false, true] {
                for high in [false, true] {
                    let c = palette(identity, dark, high);
                    for background in [
                        c.surface_canvas,
                        c.surface_panel,
                        c.surface_raised,
                        c.surface_hover,
                        c.selection_background,
                        c.action_quiet_hover,
                    ] {
                        assert!(
                            contrast(c.text_muted, background) >= if high { 7.0 } else { 4.5 },
                            "{identity:?}, dark={dark}, high={high}: muted collection metadata"
                        );
                        assert!(
                            contrast(c.border_control, background) >= 3.0,
                            "{identity:?}, dark={dark}, high={high}: meaningful control edge"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn shared_style_and_custom_tokens_resolve_identical_app_colours() {
        for light in [false, true] {
            for high_contrast in [false, true] {
                let appearance = Appearance {
                    light,
                    high_contrast,
                    ..Appearance::default()
                };
                let context = egui::Context::default();
                appearance.apply(&context);
                let tokens = appearance.tokens(!light);
                assert_eq!(
                    context.style_of(context.theme()).visuals.panel_fill,
                    egui::Color32::from(tokens.colours.surface_panel)
                );
                assert_eq!(
                    context.style_of(context.theme()).visuals.selection.bg_fill,
                    egui::Color32::from(tokens.colours.selection_background)
                );
                assert_ne!(
                    tokens.colours.selection_indicator,
                    tokens.colours.focus_ring
                );
            }
        }
    }
}
