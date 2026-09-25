//! Interface styles: the shape of the interface apart from its colours (`appearance.style`). A
//! style sets corner radii, spacing, text sizes, depth and how the work area sits in the window;
//! the theme still sets every colour. Terminals and editors draw their own grids, so no style
//! reaches inside them.

use egui::{Color32, CornerRadius, Frame, Margin, Shadow, Stroke, Vec2, vec2};

/// One of the styles `appearance.style` names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Style {
    /// throng's first look: flat columns, 6 px corners.
    Classic,
    /// Rounder and roomier, with softer shadows and pill-shaped selections.
    Soft,
    /// Dense rows and square-ish corners, for small screens.
    Compact,
    /// A darker window ground with the work set on a raised card.
    #[default]
    Elevated,
}

impl Style {
    pub const ALL: [Style; 4] = [Style::Classic, Style::Soft, Style::Compact, Style::Elevated];

    /// The style a setting names; an unknown name is the default.
    #[must_use]
    pub fn parse(name: &str) -> Self {
        Self::ALL.into_iter().find(|s| s.key().eq_ignore_ascii_case(name.trim())).unwrap_or_default()
    }

    /// The setting's value for this style.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Style::Classic => "classic",
            Style::Soft => "soft",
            Style::Compact => "compact",
            Style::Elevated => "elevated",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Style::Classic => "Classic",
            Style::Soft => "Soft",
            Style::Compact => "Compact",
            Style::Elevated => "Elevated",
        }
    }

    #[must_use]
    pub fn shape(self) -> Shape {
        match self {
            Style::Classic => Shape::CLASSIC,
            Style::Soft => Shape::SOFT,
            Style::Compact => Shape::COMPACT,
            Style::Elevated => Shape::ELEVATED,
        }
    }
}

/// A style's measures.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    /// Buttons, rows, fields and panel tabs.
    pub control_radius: u8,
    /// Windows, menus and popups.
    pub window_radius: u8,
    pub item_spacing: Vec2,
    pub button_padding: Vec2,
    /// The height a row or control is at least.
    pub row_height: f32,
    pub menu_margin: i8,
    pub window_margin: i8,
    /// Body, small, heading and interface-monospace text sizes.
    pub text: [f32; 4],
    /// Inside each side column.
    pub side_margin: Margin,
    pub status_height: f32,
    pub tab_bar_height: f32,
    /// Room around the work area. With a card, the gap between it and the window's edges.
    pub work_margin: i8,
    /// The work area is a raised card with this corner radius, on a darker ground.
    pub card_radius: Option<u8>,
    /// Shadow strength, 0 (none) to 1.
    pub depth: f32,
    /// The open workspace tab and the active project are filled pills, not underlined rows.
    pub pills: bool,
    /// Draw the line egui puts between the columns and the work area.
    pub column_lines: bool,
}

impl Shape {
    pub const CLASSIC: Shape = Shape {
        control_radius: 6,
        window_radius: 8,
        item_spacing: vec2(8.0, 4.0),
        button_padding: vec2(6.0, 3.0),
        row_height: 18.0,
        menu_margin: 6,
        window_margin: 12,
        text: [13.0, 10.5, 18.0, 12.5],
        side_margin: Margin::symmetric(8, 6),
        status_height: 24.0,
        tab_bar_height: 30.0,
        work_margin: 4,
        card_radius: None,
        depth: 1.0,
        pills: false,
        column_lines: true,
    };
    pub const SOFT: Shape = Shape {
        control_radius: 8,
        window_radius: 12,
        item_spacing: vec2(8.0, 5.0),
        button_padding: vec2(9.0, 4.0),
        row_height: 22.0,
        menu_margin: 8,
        window_margin: 16,
        text: [13.0, 10.5, 18.0, 12.5],
        side_margin: Margin::symmetric(10, 8),
        status_height: 28.0,
        tab_bar_height: 34.0,
        work_margin: 8,
        card_radius: None,
        depth: 1.4,
        pills: true,
        column_lines: true,
    };
    pub const COMPACT: Shape = Shape {
        control_radius: 2,
        window_radius: 3,
        item_spacing: vec2(6.0, 2.0),
        button_padding: vec2(4.0, 1.0),
        row_height: 16.0,
        menu_margin: 4,
        window_margin: 8,
        text: [12.0, 10.0, 16.0, 11.5],
        side_margin: Margin::symmetric(6, 4),
        status_height: 20.0,
        tab_bar_height: 24.0,
        work_margin: 2,
        card_radius: None,
        depth: 0.4,
        pills: false,
        column_lines: true,
    };
    pub const ELEVATED: Shape = Shape {
        control_radius: 7,
        window_radius: 12,
        item_spacing: vec2(8.0, 4.0),
        button_padding: vec2(8.0, 3.0),
        row_height: 20.0,
        menu_margin: 6,
        window_margin: 14,
        text: [13.0, 10.5, 18.0, 12.5],
        side_margin: Margin::symmetric(10, 8),
        status_height: 26.0,
        tab_bar_height: 32.0,
        work_margin: 8,
        card_radius: Some(12),
        depth: 1.2,
        pills: true,
        column_lines: false,
    };

    /// A shadow scaled by the style's depth.
    #[must_use]
    pub fn shadow(&self, offset: i8, blur: u8, color: Color32) -> Shadow {
        let scale = |v: f32| (v * self.depth).round();
        Shadow {
            offset: [0, scale(f32::from(offset)) as i8],
            blur: scale(f32::from(blur)).clamp(0.0, 255.0) as u8,
            spread: 0,
            color: color.gamma_multiply(self.depth.min(1.0)),
        }
    }
}

/// The grounds a style lays out: the window's own, and the work area's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grounds {
    /// Behind everything: the menu bar and the gutter around the work card.
    pub window: Color32,
    /// The work area (and egui's panel fill), which the dock's tab bars sit on.
    pub work: Color32,
    /// The work card's hairline border.
    pub edge: Color32,
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (f32::from(x) * (1.0 - t) + f32::from(y) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// For a card style the window ground is the app's ground a step darker, and the card is the
/// app's ground; where the ground is already black, the card is lifted instead.
#[must_use]
pub fn grounds(shape: &Shape, app: Color32, border: Color32, dark: bool) -> Grounds {
    if shape.card_radius.is_none() {
        return Grounds { window: app, work: app, edge: border };
    }
    let (black, white) = (Color32::BLACK, Color32::WHITE);
    let darker = mix(app, black, if dark { 0.38 } else { 0.07 });
    let lum = |c: Color32| (u32::from(c.r()) + u32::from(c.g()) + u32::from(c.b())) as f32 / 765.0;
    if (lum(app) - lum(darker)).abs() * 255.0 < 3.0 {
        Grounds { window: app, work: mix(app, white, 0.05), edge: border }
    } else {
        Grounds { window: darker, work: app, edge: border }
    }
}

/// The work area's frames: the outer one lays the window ground and the gutter, the inner one is
/// the card (or, without a card, the plain margin).
pub fn work_frames(shape: &Shape, grounds: &Grounds) -> (Frame, Frame) {
    match shape.card_radius {
        Some(radius) => (
            Frame::new().fill(grounds.window).inner_margin(Margin {
                left: shape.work_margin / 2,
                right: shape.work_margin,
                top: shape.work_margin / 2,
                bottom: shape.work_margin,
            }),
            Frame::new()
                .fill(grounds.work)
                .stroke(Stroke::new(1.0, grounds.edge))
                .corner_radius(CornerRadius::same(radius))
                .inner_margin(Margin::same(6)),
        ),
        None => (Frame::new().fill(grounds.work).inner_margin(Margin::same(shape.work_margin)), Frame::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setting_names_a_style_and_anything_else_is_elevated() {
        for style in Style::ALL {
            assert_eq!(Style::parse(style.key()), style);
            assert!(throng_core::settings::STYLES.contains(&style.key()), "{style:?} is a setting value");
        }
        assert_eq!(Style::parse("SOFT"), Style::Soft);
        assert_eq!(Style::parse("baroque"), Style::Elevated);
        assert_eq!(throng_core::settings::STYLES.len(), Style::ALL.len());
    }

    #[test]
    fn compact_is_denser_and_squarer_than_soft() {
        let (soft, compact) = (Style::Soft.shape(), Style::Compact.shape());
        assert!(compact.control_radius <= 2 && soft.control_radius >= 8);
        assert!(compact.row_height < soft.row_height && compact.item_spacing.y < soft.item_spacing.y);
        assert!(compact.status_height < soft.status_height);
    }

    #[test]
    fn a_card_sits_on_a_darker_ground_and_a_black_ground_lifts_the_card() {
        let shape = Style::Elevated.shape();
        let app = Color32::from_rgb(0x10, 0x13, 0x1a);
        let g = grounds(&shape, app, Color32::GRAY, true);
        assert_eq!(g.work, app);
        assert!(g.window.r() < app.r() && g.window.b() < app.b());
        let g = grounds(&shape, Color32::BLACK, Color32::GRAY, true);
        assert_eq!(g.window, Color32::BLACK);
        assert_ne!(g.work, Color32::BLACK);
        let flat = grounds(&Style::Classic.shape(), app, Color32::GRAY, true);
        assert_eq!((flat.window, flat.work), (app, app));
    }
}
