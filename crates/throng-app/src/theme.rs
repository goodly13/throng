//! Look and feel: the active theme's tokens turned into egui visuals, editor and terminal colours,
//! and syntax colours, laid out in the chosen interface style ([`crate::style`]). The project
//! colour stays the project's mark (its dot, its pane border); the theme owns everything else, so a
//! theme reads the same whichever project is open.

use egui::{
    Color32, Context, CornerRadius, FontFamily, FontId, Frame, Margin, RichText, Stroke, TextStyle,
    ThemePreference, Visuals,
};
use throng_core::project::Colour;
use throng_core::theme::Theme;
use throng_editor::highlight::{SyntaxColours, build_theme};

use crate::code::CodeColours;
use crate::style::{Grounds, Shape, Style};
use crate::term::colors::Palette;

pub use crate::fonts::HEADING;

/// Everything drawn from one theme in one style.
#[derive(Clone, Debug)]
pub struct Look {
    pub name: String,
    pub dark: bool,
    pub style: Style,
    pub shape: Shape,
    pub grounds: Grounds,
    pub visuals: Visuals,
    /// The side columns' ground (the projects list and the file tree).
    pub sidebar: Color32,
    pub status_bar: Color32,
    /// The theme's accent, for the bar beside an active item.
    pub accent: Color32,
    pub code: CodeColours,
    pub palette: Palette,
    pub syntax: SyntaxColours,
}

impl Look {
    #[must_use]
    pub fn new(theme: &Theme, style: Style) -> Self {
        let shape = style.shape();
        let dark = theme.is_dark();
        let grounds = crate::style::grounds(&shape, token(theme, "appBg"), token(theme, "border"), dark);
        let mut visuals = visuals(theme, &shape);
        visuals.panel_fill = grounds.work;
        Self {
            name: theme.name.clone(),
            dark,
            style,
            shape,
            grounds,
            visuals,
            sidebar: token(theme, "sidebarBg"),
            status_bar: token(theme, "statusBarBg"),
            accent: token(theme, "accent"),
            code: code_colours(theme),
            palette: palette(theme),
            syntax: syntax_colours(theme),
        }
    }

    /// The highlighting theme syntect draws with.
    #[must_use]
    pub fn syntax_theme(&self) -> syntect::highlighting::Theme {
        build_theme(&self.syntax)
    }

    /// The menu bar's frame: on the window's ground.
    pub fn menu_frame(&self) -> Frame {
        Frame::new().fill(self.grounds.window).inner_margin(Margin::symmetric(8, 2))
    }

    /// Whether the work sits on a card, with the window's chrome on a ground of its own.
    fn carded(&self) -> bool {
        self.shape.card_radius.is_some()
    }

    /// The status bar's frame: its own colour, or the window's ground around a card.
    pub fn status_frame(&self) -> Frame {
        let fill = if self.carded() { self.grounds.window } else { self.status_bar };
        Frame::new().fill(fill).inner_margin(Margin::symmetric(8, 2))
    }

    /// A side column's frame: its own surface, a shade apart from the work between the columns.
    pub fn side_frame(&self) -> Frame {
        Frame::new().fill(self.sidebar).inner_margin(self.shape.side_margin)
    }

    /// The projects column's frame. Around a card it is part of the window's chrome, so the
    /// file tree beside it stands out as a surface of its own.
    pub fn projects_frame(&self) -> Frame {
        let frame = self.side_frame();
        if self.carded() { frame.fill(self.grounds.window) } else { frame }
    }
}

impl Default for Look {
    fn default() -> Self {
        Self::new(&throng_core::theme::builtins()[0], Style::default())
    }
}

/// Put `look` on screen at `ui_scale`.
pub fn apply(ctx: &Context, look: &Look, ui_scale: f32) {
    // The theme decides light or dark; egui's own preference only picks which of its two style
    // slots is live, and both hold the theme's visuals.
    ctx.set_theme(if look.dark { ThemePreference::Dark } else { ThemePreference::Light });
    for slot in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.style_mut_of(slot, |style| {
            style.visuals = look.visuals.clone();
            proportions(style, &look.shape);
        });
    }
    if (ctx.zoom_factor() - ui_scale).abs() > f32::EPSILON {
        ctx.set_zoom_factor(ui_scale);
    }
}

/// The interface's measure: text sizes, and the room between and inside controls.
fn proportions(style: &mut egui::Style, shape: &Shape) {
    let s = &mut style.spacing;
    s.item_spacing = shape.item_spacing;
    s.button_padding = shape.button_padding;
    s.interact_size.y = shape.row_height;
    s.menu_margin = Margin::same(shape.menu_margin);
    s.window_margin = Margin::same(shape.window_margin);
    s.icon_spacing = 6.0;
    s.indent = 16.0;
    let heading = FontFamily::Name(HEADING.into());
    let [body, small, title, mono] = shape.text;
    style.text_styles = [
        (TextStyle::Small, FontId::proportional(small)),
        (TextStyle::Body, FontId::proportional(body)),
        (TextStyle::Button, FontId::proportional(body)),
        (TextStyle::Heading, FontId::new(title, heading)),
        (TextStyle::Monospace, FontId::monospace(mono)),
    ]
    .into();
}

/// A column's or group's title: small, spaced capitals in the muted text colour.
#[must_use]
pub fn section_title(ui: &egui::Ui, text: &str) -> RichText {
    RichText::new(text.to_uppercase())
        .family(FontFamily::Name(HEADING.into()))
        .size(11.0)
        .extra_letter_spacing(0.6)
        .color(ui.visuals().weak_text_color())
}

#[must_use]
pub fn to_color32(colour: Colour) -> Color32 {
    Color32::from_rgb(colour.r, colour.g, colour.b)
}

/// Text that reads on top of `colour`.
#[must_use]
pub fn on(colour: Colour) -> Color32 {
    if colour.luminance() > 0.45 { Color32::from_rgb(0x10, 0x12, 0x16) } else { Color32::WHITE }
}

fn token(theme: &Theme, key: &str) -> Color32 {
    to_color32(theme.colour(key))
}

fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (f32::from(x) * t + f32::from(y) * (1.0 - t)).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// egui's visuals from a theme's general tokens, in a style's shape.
#[must_use]
pub fn visuals(theme: &Theme, shape: &Shape) -> Visuals {
    let t = |key| token(theme, key);
    let dark = theme.is_dark();
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    let (text, muted, accent, border) = (t("text"), t("textMuted"), t("accent"), t("border"));
    let (surface, active) = (t("surface"), t("surfaceActive"));
    v.panel_fill = t("appBg");
    v.window_fill = surface;
    v.window_stroke = Stroke::new(1.0, border);
    v.extreme_bg_color = t("editorBg");
    v.text_edit_bg_color = Some(t("editorBg"));
    v.faint_bg_color = t("editorStatusStripBg");
    v.code_bg_color = active;
    v.hyperlink_color = t("linkUnderline");
    v.error_fg_color = t("danger");
    v.warn_fg_color = t("warning");
    v.weak_text_color = Some(muted);
    // A pill style fills a selection more gently: the pill's shape already marks it.
    v.selection.bg_fill = blend(accent, surface, if shape.pills { 0.32 } else { 0.45 });
    v.selection.stroke = Stroke::new(1.0, text);
    v.text_cursor.stroke.color = t("editorCursor");
    // Rounded, hairline-bordered and lifted as far as the style says.
    v.window_corner_radius = CornerRadius::same(shape.window_radius);
    v.menu_corner_radius = CornerRadius::same(shape.window_radius);
    let shadow = Color32::from_black_alpha(if dark { 90 } else { 28 });
    v.window_shadow = shape.shadow(8, 24, shadow);
    v.popup_shadow = shape.shadow(4, 12, shadow);

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = t("appBg");
    w.noninteractive.weak_bg_fill = t("appBg");
    w.noninteractive.bg_stroke = Stroke::new(1.0, border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, text);
    w.inactive.bg_fill = surface;
    w.inactive.weak_bg_fill = surface;
    w.inactive.fg_stroke = Stroke::new(1.0, text);
    w.hovered.bg_fill = active;
    w.hovered.weak_bg_fill = active;
    w.hovered.bg_stroke = Stroke::new(1.0, blend(accent, border, 0.6));
    w.hovered.fg_stroke = Stroke::new(1.5, text);
    w.active.bg_fill = blend(accent, active, 0.35);
    w.active.weak_bg_fill = blend(accent, active, 0.35);
    w.active.bg_stroke = Stroke::new(1.0, accent);
    w.active.fg_stroke = Stroke::new(2.0, text);
    w.open.bg_fill = active;
    w.open.weak_bg_fill = active;
    w.open.bg_stroke = Stroke::new(1.0, border);
    w.open.fg_stroke = Stroke::new(1.0, text);
    for state in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        state.corner_radius = CornerRadius::same(shape.control_radius);
    }
    // Controls show a frame when they are hovered or pressed, not at rest.
    w.inactive.bg_stroke = Stroke::new(1.0, blend(border, surface, 0.6));
    w.hovered.bg_stroke = Stroke::new(1.0, border);
    w.hovered.fg_stroke = Stroke::new(1.0, text);
    w.active.fg_stroke = Stroke::new(1.0, text);
    v
}

/// Editor colours from a theme's editor and search tokens.
#[must_use]
pub fn code_colours(theme: &Theme) -> CodeColours {
    let t = |key| token(theme, key);
    CodeColours {
        background: t("editorBg"),
        foreground: t("editorFg"),
        gutter_bg: t("editorGutterBg"),
        gutter_fg: t("editorGutterFg"),
        gutter_active: t("editorFg"),
        cursor: t("editorCursor"),
        selection: t("editorSelection"),
        current_line: if theme.is_dark() {
            Color32::from_white_alpha(8)
        } else {
            Color32::from_black_alpha(10)
        },
        search_match: t("searchMatch"),
        search_current: t("searchMatchCurrent"),
        search_current_border: t("searchMatchCurrentBorder"),
        link: t("linkUnderline"),
    }
}

/// The terminal palette: the theme's ground, text, cursor and selection over its own ANSI colours,
/// else throng's for a light or dark ground.
#[must_use]
pub fn palette(theme: &Theme) -> Palette {
    let t = |key| token(theme, key);
    let base = if theme.is_dark() { Palette::dark() } else { Palette::light() };
    Palette {
        foreground: t("terminalFg"),
        background: t("terminalBg"),
        cursor: t("terminalCursor"),
        selection: t("terminalSelection"),
        ansi: theme.ansi().map_or(base.ansi, |ansi| ansi.map(to_color32)),
    }
}

/// The syntax colours from a theme's syntax tokens.
#[must_use]
pub fn syntax_colours(theme: &Theme) -> SyntaxColours {
    let t = |key: &str| {
        let c = theme.colour(key);
        [c.r, c.g, c.b, 255]
    };
    SyntaxColours {
        foreground: t("editorFg"),
        keyword: t("syntaxKeyword"),
        string: t("syntaxString"),
        comment: t("syntaxComment"),
        number: t("syntaxNumber"),
        type_: t("syntaxType"),
        function: t("syntaxFunction"),
        variable: t("syntaxVariable"),
        operator: t("syntaxOperator"),
        punctuation: t("syntaxPunctuation"),
        invalid: t("syntaxInvalid"),
    }
}

/// The highlighting theme of the default look, for code that runs before any theme is chosen.
#[must_use]
pub fn default_syntax_theme() -> syntect::highlighting::Theme {
    Look::default().syntax_theme()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_light_theme_draws_light_and_its_tokens_reach_every_surface() {
        let themes = throng_core::theme::builtins();
        let light = themes.iter().find(|t| t.name == "Light").unwrap();
        let look = Look::new(light, Style::Classic);
        assert!(!look.dark && !look.visuals.dark_mode);
        assert_eq!(look.visuals.panel_fill, token(light, "appBg"));
        assert_eq!(look.code.background, token(light, "editorBg"));
        assert_eq!(look.palette.background, token(light, "terminalBg"));
        assert_eq!(
            look.syntax.keyword[..3],
            [
                light.colour("syntaxKeyword").r,
                light.colour("syntaxKeyword").g,
                light.colour("syntaxKeyword").b
            ]
        );
        let dark = Look::new(&themes[0], Style::Classic);
        assert!(dark.dark && dark.visuals.dark_mode);
        assert_ne!(dark.palette.ansi, look.palette.ansi, "ANSI colours suit the ground");
        let nord = themes.iter().find(|t| t.name == "Nord").unwrap();
        let ansi = nord.ansi().unwrap();
        assert_eq!(Look::new(nord, Style::Classic).palette.ansi[1], to_color32(ansi[1]), "its own");
    }

    #[test]
    fn a_style_shapes_the_controls_and_keeps_the_themes_colours() {
        let theme = &throng_core::theme::builtins()[0];
        let classic = Look::new(theme, Style::Classic);
        let compact = Look::new(theme, Style::Compact);
        let soft = Look::new(theme, Style::Soft);
        let elevated = Look::new(theme, Style::Elevated);
        assert_eq!(classic.visuals.widgets.inactive.corner_radius, CornerRadius::same(6));
        assert_eq!(compact.visuals.widgets.inactive.corner_radius, CornerRadius::same(2));
        assert!(soft.visuals.window_corner_radius.nw >= 12 && soft.visuals.popup_shadow.blur > 12);
        for look in [&classic, &compact, &soft] {
            assert_eq!(look.visuals.panel_fill, token(theme, "appBg"));
        }
        assert_eq!(elevated.visuals.panel_fill, token(theme, "appBg"), "the card is the app's ground");
        assert_ne!(elevated.grounds.window, elevated.grounds.work, "on a darker window ground");
        assert_eq!(elevated.code, classic.code);
        assert_eq!(elevated.palette, classic.palette);
    }
}
