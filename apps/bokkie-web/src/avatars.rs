//! Bots' avatars: Bokkie's own soft shapes with two small eyes, in solid
//! colours, drawn as vectors. Each Bot's shape and colour are picked from
//! its ID, so a Bot keeps its look on every device and across reloads with
//! nothing stored. The `avatar` a Bot carries on the wire, an emoji or short
//! text, is not drawn.
//!
//! The colours are mid-tones, at least 3:1 against the surfaces the list
//! and headers sit on in the light and dark themes and their high-contrast
//! variants; the eyes take near-black or white, whichever contrasts more.

use bokkie_protocol::BotId;
use egui::Color32;
use stobie_ui_egui::{AvatarFace, AvatarOutline, AvatarSize, AvatarSpec, OutlineSegment};

use OutlineSegment::{Cubic, Line, Quadratic};

/// One of Bokkie's shapes, with where its eyes sit.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BotShape {
    /// What the tests call it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) name: &'static str,
    pub(crate) outline: AvatarOutline<'static>,
    pub(crate) eyes: [f32; 2],
}

/// A blob: round, a little fuller on one side.
const BLOB: AvatarOutline<'static> = AvatarOutline::new(
    [0.52, 0.04],
    &[
        Cubic([0.82, 0.03], [0.98, 0.26], [0.96, 0.52]),
        Cubic([0.94, 0.80], [0.74, 0.98], [0.48, 0.96]),
        Cubic([0.20, 0.94], [0.03, 0.76], [0.05, 0.48]),
        Cubic([0.07, 0.20], [0.26, 0.05], [0.52, 0.04]),
    ],
);

/// A drop: rounded below, narrowing to a soft tip above.
const DROP: AvatarOutline<'static> = AvatarOutline::new(
    [0.44, 0.08],
    &[
        Quadratic([0.50, 0.0], [0.56, 0.08]),
        Cubic([0.70, 0.26], [0.93, 0.44], [0.93, 0.65]),
        Cubic([0.93, 0.86], [0.74, 0.98], [0.50, 0.98]),
        Cubic([0.26, 0.98], [0.07, 0.86], [0.07, 0.65]),
        Cubic([0.07, 0.44], [0.30, 0.26], [0.44, 0.08]),
    ],
);

/// A pebble: wider than tall, and a little lopsided.
const PEBBLE: AvatarOutline<'static> = AvatarOutline::new(
    [0.50, 0.13],
    &[
        Cubic([0.80, 0.11], [1.0, 0.30], [0.98, 0.54]),
        Cubic([0.96, 0.78], [0.76, 0.90], [0.48, 0.88]),
        Cubic([0.20, 0.86], [0.0, 0.74], [0.02, 0.50]),
        Cubic([0.04, 0.26], [0.22, 0.15], [0.50, 0.13]),
    ],
);

/// A squircle: a square with long, soft corners.
const SQUIRCLE: AvatarOutline<'static> = AvatarOutline::new(
    [0.50, 0.04],
    &[
        Cubic([0.90, 0.04], [0.96, 0.10], [0.96, 0.50]),
        Cubic([0.96, 0.90], [0.90, 0.96], [0.50, 0.96]),
        Cubic([0.10, 0.96], [0.04, 0.90], [0.04, 0.50]),
        Cubic([0.04, 0.10], [0.10, 0.04], [0.50, 0.04]),
    ],
);

/// A bean: round, with a gentle dip across its top.
const BEAN: AvatarOutline<'static> = AvatarOutline::new(
    [0.28, 0.10],
    &[
        Cubic([0.42, 0.04], [0.54, 0.22], [0.70, 0.12]),
        Cubic([0.88, 0.02], [1.0, 0.30], [0.96, 0.56]),
        Cubic([0.92, 0.84], [0.70, 0.97], [0.46, 0.95]),
        Cubic([0.18, 0.93], [0.0, 0.74], [0.03, 0.46]),
        Cubic([0.05, 0.22], [0.14, 0.14], [0.28, 0.10]),
    ],
);

/// A soft triangle: point up, every corner and side rounded.
const TRIANGLE: AvatarOutline<'static> = AvatarOutline::new(
    [0.40, 0.15],
    &[
        Quadratic([0.50, 0.0], [0.60, 0.15]),
        Cubic([0.74, 0.38], [0.90, 0.60], [0.95, 0.75]),
        Quadratic([1.0, 0.94], [0.82, 0.95]),
        Cubic([0.60, 0.98], [0.40, 0.98], [0.18, 0.95]),
        Quadratic([0.0, 0.94], [0.05, 0.75]),
        Cubic([0.10, 0.60], [0.26, 0.38], [0.40, 0.15]),
    ],
);

/// A cloud: three bumps over a soft, flat base.
const CLOUD: AvatarOutline<'static> = AvatarOutline::new(
    [0.14, 0.90],
    &[
        Cubic([0.0, 0.90], [0.0, 0.52], [0.16, 0.46]),
        Cubic([0.14, 0.16], [0.46, 0.04], [0.58, 0.22]),
        Cubic([0.76, 0.10], [1.0, 0.28], [0.88, 0.48]),
        Cubic([1.0, 0.56], [1.0, 0.90], [0.86, 0.90]),
        Line([0.14, 0.90]),
    ],
);

/// Bokkie's shapes. Changing their number or order changes Bots' looks.
pub(crate) const SHAPES: [BotShape; 7] = [
    BotShape {
        name: "blob",
        outline: BLOB,
        eyes: [0.5, 0.46],
    },
    BotShape {
        name: "drop",
        outline: DROP,
        eyes: [0.5, 0.64],
    },
    BotShape {
        name: "pebble",
        outline: PEBBLE,
        eyes: [0.5, 0.48],
    },
    BotShape {
        name: "squircle",
        outline: SQUIRCLE,
        eyes: [0.5, 0.46],
    },
    BotShape {
        name: "bean",
        outline: BEAN,
        eyes: [0.5, 0.52],
    },
    BotShape {
        name: "triangle",
        outline: TRIANGLE,
        eyes: [0.5, 0.66],
    },
    BotShape {
        name: "cloud",
        outline: CLOUD,
        eyes: [0.5, 0.58],
    },
];

/// Bokkie's avatar colours, all of about the same lightness, so each reads
/// on the light and the dark themes alike. Changing their number or order
/// changes Bots' looks.
pub(crate) const COLOURS: [(&str, Color32); 8] = [
    ("coral", Color32::from_rgb(208, 74, 53)),
    ("amber", Color32::from_rgb(159, 109, 37)),
    ("green", Color32::from_rgb(32, 137, 67)),
    ("teal", Color32::from_rgb(31, 133, 129)),
    ("blue", Color32::from_rgb(69, 116, 211)),
    ("violet", Color32::from_rgb(139, 93, 217)),
    ("plum", Color32::from_rgb(185, 63, 210)),
    ("rose", Color32::from_rgb(210, 62, 123)),
];

/// Which shape and colour a Bot has. Seven shapes and eight colours give
/// 56 looks, and Bots whose IDs follow one another, as a household's do
/// when made one after another, differ in both shape and colour.
pub(crate) fn pick(bot: BotId) -> (&'static BotShape, Color32) {
    let id = bot.0.rem_euclid(56) as usize;
    (&SHAPES[id % SHAPES.len()], COLOURS[id % COLOURS.len()].1)
}

/// A Bot's avatar at `size`.
pub(crate) fn bot_avatar(bot: BotId, size: AvatarSize) -> AvatarSpec<'static> {
    let (shape, colour) = pick(bot);
    AvatarSpec::shape(
        shape.outline,
        colour,
        AvatarFace::Eyes { centre: shape.eyes },
        size,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use stobie_ui_egui::{
        AvatarFigure, DensityVariant, DesignTokens, ThemeVariant, avatar_eye_colour,
    };

    use super::*;

    const THEMES: [ThemeVariant; 4] = [
        ThemeVariant::Light,
        ThemeVariant::Dark,
        ThemeVariant::LightHighContrast,
        ThemeVariant::DarkHighContrast,
    ];

    fn luminance(colour: Color32) -> f32 {
        let channel = |value: u8| {
            let value = f32::from(value) / 255.0;
            if value <= 0.040_45 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(colour.r()) + 0.7152 * channel(colour.g()) + 0.0722 * channel(colour.b())
    }

    fn contrast(first: Color32, second: Color32) -> f32 {
        let (a, b) = (luminance(first), luminance(second));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// An outline as a polygon, each curve in 24 straight pieces.
    fn polygon(outline: &AvatarOutline<'_>) -> Vec<[f32; 2]> {
        let lerp =
            |a: [f32; 2], b: [f32; 2], t: f32| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
        let mut points = vec![outline.start()];
        let mut from = outline.start();
        for segment in outline.segments() {
            let to = match *segment {
                Line(end) => {
                    points.push(end);
                    end
                }
                Quadratic(control, end) => {
                    for step in 1..=24 {
                        let t = step as f32 / 24.0;
                        points.push(lerp(lerp(from, control, t), lerp(control, end, t), t));
                    }
                    end
                }
                Cubic(first, second, end) => {
                    for step in 1..=24 {
                        let t = step as f32 / 24.0;
                        let (a, b, c) = (
                            lerp(from, first, t),
                            lerp(first, second, t),
                            lerp(second, end, t),
                        );
                        points.push(lerp(lerp(a, b, t), lerp(b, c, t), t));
                    }
                    end
                }
            };
            from = to;
        }
        points
    }

    fn inside(polygon: &[[f32; 2]], point: [f32; 2]) -> bool {
        let mut inside = false;
        let mut previous = polygon[polygon.len() - 1];
        for &current in polygon {
            if (current[1] > point[1]) != (previous[1] > point[1]) {
                let x = previous[0]
                    + (point[1] - previous[1]) * (current[0] - previous[0])
                        / (current[1] - previous[1]);
                if point[0] < x {
                    inside = !inside;
                }
            }
            previous = current;
        }
        inside
    }

    /// Which cells of a 40 by 40 grid over the unit square a shape covers.
    fn coverage(outline: &AvatarOutline<'_>) -> Vec<bool> {
        let polygon = polygon(outline);
        (0..40 * 40)
            .map(|cell| {
                let point = [
                    ((cell % 40) as f32 + 0.5) / 40.0,
                    ((cell / 40) as f32 + 0.5) / 40.0,
                ];
                inside(&polygon, point)
            })
            .collect()
    }

    #[test]
    fn a_bot_keeps_its_avatar() {
        // The same ID gives the same shape and colour every time, as on
        // another device or after a reload.
        for id in [1, 2, 3, 42, 1_000, 9_007_199_254_740_991] {
            assert_eq!(
                bot_avatar(BotId(id), AvatarSize::List),
                bot_avatar(BotId(id), AvatarSize::List)
            );
        }
        // Fixed IDs keep the looks they have today.
        let look = |id| {
            let (shape, colour) = pick(BotId(id));
            let name = COLOURS.iter().find(|(_, c)| *c == colour).unwrap().0;
            (shape.name, name)
        };
        assert_eq!(
            [1, 2, 3, 57].map(look),
            [
                ("drop", "amber"),
                ("pebble", "green"),
                ("squircle", "teal"),
                ("drop", "amber"),
            ],
            "a change here changes the look of existing Bots"
        );
        // The size is the caller's; the look is the Bot's.
        let spec = bot_avatar(BotId(7), AvatarSize::Strip);
        assert_eq!(spec.size, AvatarSize::Strip);
        let AvatarFigure::Shape { fill, face, .. } = spec.figure else {
            panic!("a Bot's avatar is a shape");
        };
        assert_eq!(fill, pick(BotId(7)).1);
        assert_eq!(
            face,
            AvatarFace::Eyes {
                centre: pick(BotId(7)).0.eyes
            }
        );
    }

    #[test]
    fn bots_made_one_after_another_look_different() {
        // 56 Bots in a row each have a look of their own, and each pair of
        // neighbours differs in both shape and colour.
        let looks: BTreeSet<(&str, [u8; 4])> = (100..156)
            .map(|id| {
                let (shape, colour) = pick(BotId(id));
                (shape.name, colour.to_array())
            })
            .collect();
        assert_eq!(looks.len(), SHAPES.len() * COLOURS.len());
        for id in 1..200 {
            let (first, second) = (pick(BotId(id)), pick(BotId(id + 1)));
            assert_ne!(first.0.name, second.0.name, "{id}");
            assert_ne!(first.1, second.1, "{id}");
        }
        let names: BTreeSet<&str> = SHAPES.iter().map(|shape| shape.name).collect();
        assert_eq!(names.len(), SHAPES.len());
        let colours: BTreeSet<[u8; 4]> = COLOURS.iter().map(|(_, c)| c.to_array()).collect();
        assert_eq!(colours.len(), COLOURS.len());
    }

    #[test]
    fn the_shapes_are_soft_distinct_and_hold_their_eyes() {
        let covered: Vec<Vec<bool>> = SHAPES
            .iter()
            .map(|shape| coverage(&shape.outline))
            .collect();
        for (shape, cells) in SHAPES.iter().zip(&covered) {
            let area = cells.iter().filter(|&&cell| cell).count() as f32 / cells.len() as f32;
            // Each fills most of its square, as a circle fills 79% of it.
            assert!((0.5..0.9).contains(&area), "{}: {area}", shape.name);
            // Both eyes, a twelfth of the avatar wide and a sixth tall with
            // their centres a quarter apart, lie wholly within the shape,
            // with room around them.
            let polygon = polygon(&shape.outline);
            let [x, y] = shape.eyes;
            for eye in [x - 0.125, x + 0.125] {
                for (dx, dy) in [(-0.1, -0.15), (0.1, -0.15), (-0.1, 0.15), (0.1, 0.15)] {
                    assert!(
                        inside(&polygon, [eye + dx, y + dy]),
                        "{}: eyes at {:?}",
                        shape.name,
                        shape.eyes
                    );
                }
            }
        }
        for (a, first) in SHAPES.iter().enumerate() {
            for (b, second) in SHAPES.iter().enumerate().skip(a + 1) {
                let differ = covered[a]
                    .iter()
                    .zip(&covered[b])
                    .filter(|(x, y)| x != y)
                    .count();
                assert!(
                    differ as f32 / covered[a].len() as f32 > 0.06,
                    "{} and {} look alike: {differ} cells differ",
                    first.name,
                    second.name
                );
            }
        }
    }

    #[test]
    fn every_colour_reads_in_both_themes() {
        for theme in THEMES {
            let colours = DesignTokens::resolve(theme, DensityVariant::Comfortable).colours;
            let surfaces = [
                ("canvas", colours.surface_canvas),
                ("panel", colours.surface_panel),
                ("raised", colours.surface_raised),
            ];
            for (name, colour) in COLOURS {
                for (surface, behind) in surfaces {
                    let ratio = contrast(colour, behind.into());
                    assert!(ratio >= 3.0, "{name} on {theme:?} {surface}: {ratio:.2}");
                }
                // A selected or hovered row's fill is quieter, but the
                // avatar still stands out from it.
                let ratio = contrast(colour, colours.selection_background.into());
                assert!(ratio >= 2.0, "{name} on {theme:?} selection: {ratio:.2}");
            }
        }
        for (name, colour) in COLOURS {
            // The eyes, near-black or white, read on every fill.
            let ratio = contrast(colour, avatar_eye_colour(colour));
            assert!(ratio >= 4.0, "{name}'s eyes: {ratio:.2}");
        }
    }
}
