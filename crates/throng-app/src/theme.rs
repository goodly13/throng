//! Look and feel: the active theme's tokens turned into egui visuals, editor and terminal colours,
//! and syntax colours. The project colour stays the project's mark (its dot, its pane border); the
//! theme owns everything else, so a theme reads the same whichever project is open.

use egui::{Color32, Context, Stroke, ThemePreference, Visuals};
use throng_core::project::Colour;
use throng_core::theme::Theme;
use throng_editor::highlight::{SyntaxColours, build_theme};

use crate::code::CodeColours;
use crate::term::colors::Palette;

/// Everything drawn from one theme.
#[derive(Clone, Debug)]
pub struct Look {
    pub name: String,
    pub dark: bool,
    pub visuals: Visuals,
    pub code: CodeColours,
    pub palette: Palette,
    pub syntax: SyntaxColours,
}

impl Look {
    #[must_use]
    pub fn new(theme: &Theme) -> Self {
        Self {
            name: theme.name.clone(),
            dark: theme.is_dark(),
            visuals: visuals(theme),
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
}

impl Default for Look {
    fn default() -> Self {
        Self::new(&throng_core::theme::builtins()[0])
    }
}

/// Put `look` on screen at `ui_scale`.
pub fn apply(ctx: &Context, look: &Look, ui_scale: f32) {
    // The theme decides light or dark; egui's own preference only picks which of its two style
    // slots is live, and both hold the theme's visuals.
    ctx.set_theme(if look.dark { ThemePreference::Dark } else { ThemePreference::Light });
    for slot in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.style_mut_of(slot, |style| style.visuals = look.visuals.clone());
    }
    if (ctx.zoom_factor() - ui_scale).abs() > f32::EPSILON {
        ctx.set_zoom_factor(ui_scale);
    }
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

/// egui's visuals from a theme's general tokens.
#[must_use]
pub fn visuals(theme: &Theme) -> Visuals {
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
    v.selection.bg_fill = blend(accent, surface, 0.45);
    v.selection.stroke = Stroke::new(1.0, text);
    v.text_cursor.stroke.color = t("editorCursor");

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

/// The terminal palette: the theme's ground, text, cursor and selection over the ANSI colours for
/// a light or dark ground.
#[must_use]
pub fn palette(theme: &Theme) -> Palette {
    let t = |key| token(theme, key);
    let base = if theme.is_dark() { Palette::dark() } else { Palette::light() };
    Palette {
        foreground: t("terminalFg"),
        background: t("terminalBg"),
        cursor: t("terminalCursor"),
        selection: t("terminalSelection"),
        ..base
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
        let look = Look::new(light);
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
        let dark = Look::new(&themes[0]);
        assert!(dark.dark && dark.visuals.dark_mode);
        assert_ne!(dark.palette.ansi, look.palette.ansi, "ANSI colours suit the ground");
    }
}
