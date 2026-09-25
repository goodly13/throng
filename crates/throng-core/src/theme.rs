//! Themes: named sets of colour tokens, the ones the app draws with. Twenty-eight ship built in —
//! `throng` and twenty-seven derived from a palette — and a user's own live as JSON files beside
//! the settings. A theme file may name only some tokens; the rest come from `throng`, so a partial
//! theme always draws. A theme may also carry its own sixteen terminal colours (`ansi`); one that
//! does not uses throng's for a light or dark ground.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::project::Colour;

mod palettes;

/// A token a theme sets, and where the preferences editor groups it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenDef {
    pub key: &'static str,
    pub group: &'static str,
    pub label: &'static str,
}

const fn token(group: &'static str, key: &'static str, label: &'static str) -> TokenDef {
    TokenDef { key, group, label }
}

/// Every token, in the editor's order.
pub const TOKENS: &[TokenDef] = &[
    token("General", "appBg", "App background"),
    token("General", "sidebarBg", "Sidebar background"),
    token("General", "surface", "Surface"),
    token("General", "surfaceActive", "Active surface"),
    token("General", "text", "Text"),
    token("General", "textMuted", "Muted text"),
    token("General", "accent", "Accent"),
    token("General", "danger", "Danger"),
    token("General", "success", "Success"),
    token("General", "warning", "Warning"),
    token("General", "border", "Border"),
    token("General", "statusBarBg", "Status bar"),
    token("General", "unsavedDot", "Unsaved marker"),
    token("General", "linkUnderline", "Link underline"),
    token("Editor", "editorBg", "Background"),
    token("Editor", "editorFg", "Text"),
    token("Editor", "editorCursor", "Caret"),
    token("Editor", "editorSelection", "Selection"),
    token("Editor", "editorGutterBg", "Gutter"),
    token("Editor", "editorGutterFg", "Line numbers"),
    token("Editor", "editorStatusStripBg", "Status strip"),
    token("Editor", "editorStatusStripFg", "Status strip text"),
    token("Syntax", "syntaxKeyword", "Keyword"),
    token("Syntax", "syntaxString", "String"),
    token("Syntax", "syntaxComment", "Comment"),
    token("Syntax", "syntaxNumber", "Number"),
    token("Syntax", "syntaxType", "Type"),
    token("Syntax", "syntaxFunction", "Function"),
    token("Syntax", "syntaxVariable", "Variable"),
    token("Syntax", "syntaxOperator", "Operator"),
    token("Syntax", "syntaxPunctuation", "Punctuation"),
    token("Syntax", "syntaxInvalid", "Invalid"),
    token("Terminal", "terminalBg", "Background"),
    token("Terminal", "terminalFg", "Text"),
    token("Terminal", "terminalCursor", "Cursor"),
    token("Terminal", "terminalSelection", "Selection"),
    token("Search", "searchMatch", "Match"),
    token("Search", "searchMatchCurrent", "Current match"),
    token("Search", "searchMatchCurrentBorder", "Current match border"),
];

/// The name of the theme everything falls back to.
pub const DEFAULT_THEME: &str = "throng";

/// A theme: a name and a colour for every token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub name: String,
    colours: BTreeMap<&'static str, Colour>,
    /// The terminal's sixteen ANSI colours, when the theme sets its own.
    ansi: Option<[Colour; 16]>,
    /// Shipped with throng, so never written or deleted.
    pub builtin: bool,
}

impl Theme {
    /// The terminal's sixteen ANSI colours, when the theme sets its own.
    #[must_use]
    pub fn ansi(&self) -> Option<[Colour; 16]> {
        self.ansi
    }

    /// A token's colour. Every token is always present.
    #[must_use]
    pub fn colour(&self, key: &str) -> Colour {
        self.colours.get(key).copied().unwrap_or(Colour { r: 255, g: 0, b: 255 })
    }

    pub fn set(&mut self, key: &str, colour: Colour) {
        if let Some(def) = TOKENS.iter().find(|t| t.key == key) {
            self.colours.insert(def.key, colour);
        }
    }

    /// A dark theme draws light text on a dark ground.
    #[must_use]
    pub fn is_dark(&self) -> bool {
        self.colour("appBg").luminance() < 0.4
    }

    /// Read a theme file: `{ "name": …, "colours": { token: "#rrggbb" }, "ansi": [16 × "#rrggbb"] }`.
    /// Tokens it does not set come from `base`, and so does `ansi` unless the file gives all
    /// sixteen; unknown tokens and other keys are left alone (and kept by [`Self::write_into`]).
    pub fn parse(text: &str, base: &Theme) -> Result<Self, String> {
        let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .ok_or("A theme needs a \"name\".")?;
        let mut theme =
            Theme { name: name.to_owned(), colours: base.colours.clone(), ansi: base.ansi, builtin: false };
        if let Some(colours) = value.get("colours").and_then(Value::as_object) {
            for (key, raw) in colours {
                if let Some(colour) = raw.as_str().and_then(|s| Colour::parse(s).ok()) {
                    theme.set(key, colour);
                }
            }
        }
        if let Some(ansi) = value.get("ansi").and_then(Value::as_array) {
            let parsed: Vec<Colour> =
                ansi.iter().filter_map(|v| v.as_str().and_then(|s| Colour::parse(s).ok())).collect();
            if let Ok(all) = <[Colour; 16]>::try_from(parsed) {
                theme.ansi = Some(all);
            }
        }
        Ok(theme)
    }

    /// This theme written over an existing file's JSON, keeping whatever else that file holds.
    #[must_use]
    pub fn write_into(&self, existing: Option<&str>) -> String {
        let mut doc = existing
            .and_then(|t| serde_json::from_str::<Value>(t).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        doc.insert("name".into(), Value::String(self.name.clone()));
        let mut colours = doc.get("colours").and_then(Value::as_object).cloned().unwrap_or_else(Map::new);
        for (key, colour) in &self.colours {
            colours.insert((*key).to_owned(), Value::String(colour.to_hex()));
        }
        doc.insert("colours".into(), Value::Object(colours));
        if let Some(ansi) = self.ansi {
            doc.insert("ansi".into(), Value::Array(ansi.iter().map(|c| Value::String(c.to_hex())).collect()));
        }
        let mut text = serde_json::to_string_pretty(&Value::Object(doc)).expect("a JSON object serialises");
        text.push('\n');
        text
    }

    /// A copy under a new name, for editing (a built-in is never edited in place).
    #[must_use]
    pub fn cloned_as(&self, name: &str) -> Self {
        Theme { name: name.to_owned(), colours: self.colours.clone(), ansi: self.ansi, builtin: false }
    }
}

/// The file name a theme is stored under: its name with anything a file name cannot hold replaced.
#[must_use]
pub fn file_name(name: &str) -> String {
    let safe: String =
        name.chars().map(|c| if c.is_alphanumeric() || " -_.".contains(c) { c } else { '-' }).collect();
    format!("{}.json", safe.trim())
}

/// The theme called `name` (case-insensitively), else the default. The old appearance values
/// `dark` and `light` name `throng` and `Light`; `system` follows the platform.
#[must_use]
pub fn resolve<'a>(themes: &'a [Theme], name: &str, system_dark: bool) -> &'a Theme {
    let wanted = match name.trim() {
        "" | "dark" => DEFAULT_THEME,
        "light" => "Light",
        "system" if system_dark => DEFAULT_THEME,
        "system" => "Light",
        other => other,
    };
    themes
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(wanted))
        .or_else(|| themes.iter().find(|t| t.name == DEFAULT_THEME))
        .unwrap_or(&themes[0])
}

fn c(hex: &str) -> Colour {
    Colour::parse(hex).expect("a built-in colour parses")
}

fn mix(a: Colour, b: Colour, t: f32) -> Colour {
    let m = |x: u8, y: u8| (f32::from(x) * t + f32::from(y) * (1.0 - t)).round().clamp(0.0, 255.0) as u8;
    Colour { r: m(a.r, b.r), g: m(a.g, b.g), b: m(a.b, b.b) }
}

/// WCAG contrast ratio between two colours.
#[must_use]
pub fn contrast(a: Colour, b: Colour) -> f32 {
    let (la, lb) = (a.luminance(), b.luminance());
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// `a` moved toward `b` by `t` (0 is `a`, 1 is `b`).
fn blend(a: Colour, b: Colour, t: f32) -> Colour {
    mix(b, a, t)
}

/// `colour`, moved toward `text` in steps of 0.05 until it reads at `min` on every ground.
fn legible_on(colour: Colour, grounds: &[Colour], text: Colour, min: f32) -> Colour {
    let mut out = colour;
    for step in 1..=20 {
        if grounds.iter().all(|g| contrast(out, *g) >= min) {
            break;
        }
        out = blend(colour, text, step as f32 * 0.05);
    }
    out
}

/// A gutter: the editor ground lifted 9% toward white on a dark theme, 6% toward black on a
/// light one, so it reads as a strip of its own.
fn gutter_for(ground: Colour) -> Colour {
    if ground.luminance() < 0.4 {
        blend(ground, Colour { r: 255, g: 255, b: 255 }, 0.09)
    } else {
        blend(ground, Colour { r: 0, g: 0, b: 0 }, 0.06)
    }
}

/// CIE L*a*b* (D65) of an sRGB colour.
fn lab(c: Colour) -> (f64, f64, f64) {
    let lin = |v: u8| {
        let s = f64::from(v) / 255.0;
        if s <= 0.039_28 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = (lin(c.r), lin(c.g), lin(c.b));
    let x = r * 0.412_456_4 + g * 0.357_576_1 + b * 0.180_437_5;
    let y = r * 0.212_672_9 + g * 0.715_152_2 + b * 0.072_175;
    let z = r * 0.019_333_9 + g * 0.119_192 + b * 0.950_304_1;
    let f = |t: f64| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0) * t / 116.0 + 16.0 / 116.0 };
    let (fx, fy, fz) = (f(x / 0.950_47), f(y), f(z / 1.088_83));
    (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
}

/// CIEDE2000 colour difference: how different two colours look.
#[must_use]
pub fn delta_e(a: Colour, b: Colour) -> f64 {
    let ((l1, a1, b1), (l2, a2, b2)) = (lab(a), lab(b));
    let c_bar7 = ((a1.hypot(b1) + a2.hypot(b2)) / 2.0).powi(7);
    let g = 0.5 * (1.0 - (c_bar7 / (c_bar7 + 25f64.powi(7))).sqrt());
    let (a1p, a2p) = ((1.0 + g) * a1, (1.0 + g) * a2);
    let (c1p, c2p) = (a1p.hypot(b1), a2p.hypot(b2));
    let hue = |y: f64, x: f64| {
        if x == 0.0 && y == 0.0 {
            0.0
        } else {
            let h = y.atan2(x).to_degrees();
            if h >= 0.0 { h } else { h + 360.0 }
        }
    };
    let (h1p, h2p) = (hue(b1, a1p), hue(b2, a2p));
    let dhp = if c1p * c2p == 0.0 {
        0.0
    } else {
        let d = h2p - h1p;
        if d.abs() <= 180.0 {
            d
        } else if d > 180.0 {
            d - 360.0
        } else {
            d + 360.0
        }
    };
    let (dlp, dcp) = (l2 - l1, c2p - c1p);
    let dhp_big = 2.0 * (c1p * c2p).sqrt() * (dhp.to_radians() / 2.0).sin();
    let l_bar = (l1 + l2) / 2.0;
    let c_bar_p = (c1p + c2p) / 2.0;
    let h_bar = if c1p * c2p == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= 180.0 {
        (h1p + h2p) / 2.0
    } else if h1p + h2p < 360.0 {
        (h1p + h2p + 360.0) / 2.0
    } else {
        (h1p + h2p - 360.0) / 2.0
    };
    let t = 1.0 - 0.17 * (h_bar - 30.0).to_radians().cos()
        + 0.24 * (2.0 * h_bar).to_radians().cos()
        + 0.32 * (3.0 * h_bar + 6.0).to_radians().cos()
        - 0.2 * (4.0 * h_bar - 63.0).to_radians().cos();
    let d_theta = 30.0 * (-((h_bar - 275.0) / 25.0).powi(2)).exp();
    let c7 = c_bar_p.powi(7);
    let rc = 2.0 * (c7 / (c7 + 25f64.powi(7))).sqrt();
    let sl = 1.0 + 0.015 * (l_bar - 50.0).powi(2) / (20.0 + (l_bar - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * c_bar_p;
    let sh = 1.0 + 0.015 * c_bar_p * t;
    let rt = -(2.0 * d_theta).to_radians().sin() * rc;
    ((dlp / sl).powi(2) + (dcp / sc).powi(2) + (dhp_big / sh).powi(2) + rt * (dcp / sc) * (dhp_big / sh))
        .sqrt()
}

/// The theme's syntax colours and the search surfaces they are read on, derived together. The hues are lifted to 6:1 on the
/// editor body first, which keeps each theme's character and leaves headroom; the match surfaces
/// are then tinted only as far as every hue, and the body text, still reads at 4.5:1 through them.
fn syntax_and_search(
    seeds: [Colour; 10],
    bg: Colour,
    fg: Colour,
    accent: Colour,
) -> ([Colour; 10], [Colour; 3]) {
    let on_body = seeds.map(|c| legible_on(c, &[bg], fg, 6.0));
    let readable = |surface: Colour| {
        contrast(fg, surface) >= 4.5 && on_body.iter().all(|c| contrast(*c, surface) >= 4.5)
    };
    // The current match: the strongest accent tint the code survives.
    let strongest =
        (1..=14).rev().map(|n| n as f32 * 0.05).find(|t| readable(blend(bg, accent, *t))).unwrap_or(0.05);
    let current = blend(bg, accent, strongest);
    let (current_seen, current_contrast) = (delta_e(current, bg), contrast(current, bg));
    // An ordinary match: on the neutral axis, readable, quieter than the current one both ways, and
    // as far from both the page and the current match as that allows.
    let mut matched = blend(bg, fg, 0.01);
    let mut best = -1.0;
    for step in (1..=70).rev() {
        let candidate = blend(bg, fg, step as f32 / 100.0);
        let quieter = delta_e(candidate, bg) <= current_seen && contrast(candidate, bg) < current_contrast;
        if !readable(candidate) || !quieter {
            continue;
        }
        let score = delta_e(candidate, current).min(delta_e(candidate, bg));
        if score > best {
            best = score;
            matched = candidate;
        }
    }
    let mut border = accent;
    for step in 0..=10 {
        if contrast(border, current) >= 3.0 {
            break;
        }
        border = blend(accent, fg, step as f32 * 0.1);
    }
    // A last pass against the match surfaces: ordinarily a no-op, it moves only a hue no usable
    // tint can carry.
    let syntax = on_body.map(|c| legible_on(c, &[bg, matched, current], fg, 4.5));
    (syntax, [matched, current, border])
}

/// What a derived theme is made from .
struct Palette {
    bg: &'static str,
    sidebar: Option<&'static str>,
    surface: &'static str,
    surface_active: Option<&'static str>,
    text: &'static str,
    text_muted: Option<&'static str>,
    accent: &'static str,
    danger: Option<&'static str>,
    success: Option<&'static str>,
    border: Option<&'static str>,
    status_bar: Option<&'static str>,
    terminal_bg: Option<&'static str>,
    terminal_fg: Option<&'static str>,
    editor_bg: Option<&'static str>,
    editor_fg: Option<&'static str>,
    selection: Option<&'static str>,
    unsaved: Option<&'static str>,
    /// keyword, string, comment, number, type, function, variable, operator, punctuation, invalid.
    syntax: [&'static str; 10],
    /// The palette's own terminal colours: black, red, green, yellow, blue, magenta, cyan, white,
    /// then the bright eight.
    ansi: Option<[&'static str; 16]>,
}

const P: Palette = Palette {
    bg: "#000000",
    sidebar: None,
    surface: "#000000",
    surface_active: None,
    text: "#ffffff",
    text_muted: None,
    accent: "#ffffff",
    danger: None,
    success: None,
    border: None,
    status_bar: None,
    terminal_bg: None,
    terminal_fg: None,
    editor_bg: None,
    editor_fg: None,
    selection: None,
    unsaved: None,
    syntax: ["#ffffff"; 10],
    ansi: None,
};

/// A palette's terminal colours made readable on the terminal's ground: each colour a program
/// writes text in reaches 4.5:1 (bright black, used for dim text, 3:1), moved toward the
/// terminal's text colour as far as that takes. The ground's own end of the scale (black on a dark
/// ground, the two whites on a light one) is a background colour and is left alone.
fn terminal_ansi(raw: [Colour; 16], ground: Colour, text: Colour) -> [Colour; 16] {
    let dark = ground.luminance() < 0.4;
    let mut out = raw;
    for (index, colour) in out.iter_mut().enumerate() {
        let background = if dark { index == 0 } else { index == 7 || index == 15 };
        if background {
            continue;
        }
        let min = if index == 8 { 3.0 } else { 4.5 };
        *colour = legible_on(*colour, &[ground], text, min);
    }
    out
}

const SYNTAX_KEYS: [&str; 10] = [
    "syntaxKeyword",
    "syntaxString",
    "syntaxComment",
    "syntaxNumber",
    "syntaxType",
    "syntaxFunction",
    "syntaxVariable",
    "syntaxOperator",
    "syntaxPunctuation",
    "syntaxInvalid",
];

fn make(name: &str, p: &Palette) -> Theme {
    let bg = c(p.bg);
    let text = c(p.text);
    let accent = c(p.accent);
    let surface = c(p.surface);
    let surface_active = p.surface_active.map_or(surface, c);
    let sidebar = p.sidebar.map_or(bg, c);
    // Muted text still reads at 4.5:1 on the grounds it is set on.
    let muted = legible_on(p.text_muted.map_or(text, c), &[bg, sidebar], text, 4.5);
    let editor_bg = p.editor_bg.map_or(bg, c);
    let editor_fg = p.editor_fg.map_or(text, c);
    let selection = p.selection.map_or(surface_active, c);
    let gutter = gutter_for(editor_bg);
    let mut colours = BTreeMap::new();
    let mut put = |k: &'static str, v: Colour| {
        colours.insert(k, v);
    };
    put("appBg", bg);
    put("sidebarBg", sidebar);
    put("surface", surface);
    put("surfaceActive", surface_active);
    put("text", text);
    put("textMuted", muted);
    put("accent", accent);
    put("danger", c(p.danger.unwrap_or("#e5534b")));
    put("success", c(p.success.unwrap_or("#3fb950")));
    put("warning", c("#d29922"));
    put("border", p.border.map_or(surface, c));
    put("statusBarBg", p.status_bar.map_or(bg, c));
    put("unsavedDot", c(p.unsaved.unwrap_or("#e3b341")));
    put("linkUnderline", accent);
    put("editorBg", editor_bg);
    put("editorFg", editor_fg);
    put("editorCursor", accent);
    put("editorSelection", selection);
    put("editorGutterBg", gutter);
    put("editorGutterFg", legible_on(muted, &[gutter], text, 4.5));
    put("editorStatusStripBg", gutter);
    put("editorStatusStripFg", legible_on(muted, &[gutter], text, 4.5));
    put("terminalBg", p.terminal_bg.map_or(bg, c));
    put("terminalFg", p.terminal_fg.map_or(text, c));
    put("terminalCursor", accent);
    put("terminalSelection", selection);
    let (syntax, [matched, current, border]) =
        syntax_and_search(p.syntax.map(c), editor_bg, editor_fg, accent);
    put("searchMatch", matched);
    put("searchMatchCurrent", current);
    put("searchMatchCurrentBorder", border);
    for (key, colour) in SYNTAX_KEYS.iter().zip(syntax) {
        put(key, colour);
    }
    let terminal = (colours["terminalBg"], colours["terminalFg"]);
    let ansi = p.ansi.map(|raw| terminal_ansi(raw.map(c), terminal.0, terminal.1));
    Theme { name: name.to_owned(), colours, ansi, builtin: true }
}

/// The hand-written `throng` theme .
fn throng() -> Theme {
    let pairs: &[(&'static str, &str)] = &[
        ("appBg", "#10131a"),
        ("sidebarBg", "#161b25"),
        ("surface", "#1b2230"),
        ("surfaceActive", "#222c3d"),
        ("text", "#e6ebf2"),
        ("textMuted", "#93a0b4"),
        ("accent", "#6aa3ff"),
        ("danger", "#e5534b"),
        ("success", "#3fb950"),
        ("warning", "#d29922"),
        ("border", "#2a3344"),
        ("statusBarBg", "#10131a"),
        ("unsavedDot", "#e3b341"),
        ("linkUnderline", "#6aa3ff"),
        ("editorBg", "#0c0f16"),
        ("editorFg", "#d6deea"),
        ("editorCursor", "#6aa3ff"),
        ("editorSelection", "#2a3a57"),
        ("editorGutterBg", "#151a23"),
        ("editorGutterFg", "#8b98ac"),
        ("editorStatusStripBg", "#151a23"),
        ("editorStatusStripFg", "#a7b4c8"),
        ("syntaxKeyword", "#7ea8ff"),
        ("syntaxString", "#8ed09a"),
        ("syntaxComment", "#93a2b8"),
        ("syntaxNumber", "#e0a878"),
        ("syntaxType", "#63cfd4"),
        ("syntaxFunction", "#c8a6f0"),
        ("syntaxVariable", "#d6deea"),
        ("syntaxOperator", "#9fb3cc"),
        ("syntaxPunctuation", "#a3b0c4"),
        ("syntaxInvalid", "#ff6b6b"),
        ("terminalBg", "#0c0f16"),
        ("terminalFg", "#d6deea"),
        ("terminalCursor", "#6aa3ff"),
        ("terminalSelection", "#2a3a57"),
        ("searchMatch", "#262a32"),
        ("searchMatchCurrent", "#213049"),
        ("searchMatchCurrentBorder", "#6aa3ff"),
    ];
    Theme {
        name: DEFAULT_THEME.to_owned(),
        colours: pairs.iter().map(|(k, v)| (*k, c(v))).collect(),
        ansi: None,
        builtin: true,
    }
}

/// The twenty-eight themes throng ships, `throng` first: its first fifteen, then the ones
/// converted from community and developer palettes ([`palettes`]).
#[must_use]
pub fn builtins() -> Vec<Theme> {
    let mut out = first_fifteen();
    out.extend(palettes::PALETTES.iter().map(|(name, palette)| make(name, palette)));
    out
}

#[rustfmt::skip]
fn first_fifteen() -> Vec<Theme> {
    let mut out = vec![throng()];
    let palettes: [(&str, Palette); 14] = [
        ("Light", Palette {
            bg: "#f5f6f8", sidebar: Some("#eceef2"), surface: "#ffffff", surface_active: Some("#e4e8ef"),
            text: "#1a1d23", text_muted: Some("#5b6470"), accent: "#2563eb", border: Some("#d5dae2"),
            status_bar: Some("#e4e8ef"), terminal_bg: Some("#ffffff"), terminal_fg: Some("#1a1d23"),
            editor_bg: Some("#ffffff"), editor_fg: Some("#1a1d23"), selection: Some("#cfe0ff"),
            syntax: ["#8250df", "#0a6b2e", "#5b6470", "#8a4b00", "#0550ae", "#7c2d91", "#1a1d23", "#374151", "#57606a", "#b91c1c"],
            ..P
        }),
        ("Snake", Palette {
            bg: "#1a1e14", sidebar: Some("#141810"), surface: "#242a1c", surface_active: Some("#323a28"),
            text: "#c8d0b0", text_muted: Some("#8b936f"), accent: "#8a9a5b", border: Some("#39412a"),
            terminal_bg: Some("#12160c"), terminal_fg: Some("#c8d0b0"), selection: Some("#3a4426"),
            syntax: ["#b3c66a", "#d3c98a", "#7b8560", "#e0b070", "#9fd0a0", "#d8dfa8", "#c8d0b0", "#a8b48a", "#8b936f", "#d9705a"],
            ..P
        }),
        ("Gothic", Palette {
            bg: "#140f16", sidebar: Some("#0e0a10"), surface: "#211a24", surface_active: Some("#2f2434"),
            text: "#d8c8d0", text_muted: Some("#8b7d86"), accent: "#8b1a2f", danger: Some("#c0392b"),
            border: Some("#332738"), terminal_bg: Some("#0f0b11"), selection: Some("#3a2540"),
            syntax: ["#c96a86", "#b9a2c9", "#7d6c78", "#d29a6a", "#9ec5c9", "#e0b7c6", "#d8c8d0", "#a893a0", "#8b7d86", "#e05252"],
            ..P
        }),
        ("Windows Terminal", Palette {
            bg: "#0c0c0c", sidebar: Some("#0c0c0c"), surface: "#1b1b1b", surface_active: Some("#2a2a2a"),
            text: "#cccccc", text_muted: Some("#8a8a8a"), accent: "#3a96dd", border: Some("#2a2a2a"),
            terminal_bg: Some("#0c0c0c"), terminal_fg: Some("#cccccc"), selection: Some("#264f78"),
            syntax: ["#61afef", "#98c379", "#7f7f7f", "#d19a66", "#56b6c2", "#c8a2e0", "#cccccc", "#abb2bf", "#9a9a9a", "#e06c75"],
            ..P
        }),
        ("Bash", Palette {
            bg: "#000000", sidebar: Some("#000000"), surface: "#101010", surface_active: Some("#1c1c1c"),
            text: "#d7d7d7", text_muted: Some("#8a8a8a"), accent: "#2bd4ee", danger: Some("#d160c9"),
            success: Some("#2ecc71"), border: Some("#1aa08a"), status_bar: Some("#000000"),
            terminal_bg: Some("#000000"), terminal_fg: Some("#d7d7d7"), editor_bg: Some("#000000"),
            editor_fg: Some("#d7d7d7"), selection: Some("#264f4a"), unsaved: Some("#e5c07b"),
            syntax: ["#d160c9", "#2ecc71", "#7a7a7a", "#e5c07b", "#2bd4ee", "#5fd7ff", "#d7d7d7", "#b0b0b0", "#9a9a9a", "#ff5f5f"],
            ..P
        }),
        ("SUBNET", Palette {
            bg: "#001B40", sidebar: Some("#001330"), surface: "#303841", surface_active: Some("#3b4753"),
            text: "#d6e6f5", text_muted: Some("#9fb0c2"), accent: "#39FF14", danger: Some("#FF6F32"),
            success: Some("#39FF14"), border: Some("#4C4C4C"), status_bar: Some("#001330"),
            terminal_bg: Some("#001B40"), terminal_fg: Some("#d6e6f5"), editor_bg: Some("#001B40"),
            editor_fg: Some("#d6e6f5"), selection: Some("#0a3350"), unsaved: Some("#FFE600"),
            syntax: ["#39FF14", "#FFE600", "#7d92a8", "#FF6F32", "#00EFFF", "#8CFF6B", "#d6e6f5", "#a8c4dc", "#9fb0c2", "#FF3B4E"],
            ..P
        }),
        ("VSCode", Palette {
            bg: "#1e1e1e", sidebar: Some("#252526"), surface: "#2d2d2d", surface_active: Some("#37373d"),
            text: "#d4d4d4", text_muted: Some("#858585"), accent: "#007acc", border: Some("#333333"),
            terminal_bg: Some("#1e1e1e"), terminal_fg: Some("#d4d4d4"), editor_bg: Some("#1e1e1e"),
            selection: Some("#264f78"),
            syntax: ["#569cd6", "#ce9178", "#6a9955", "#b5cea8", "#4ec9b0", "#dcdcaa", "#9cdcfe", "#d4d4d4", "#a0a0a0", "#f44747"],
            ..P
        }),
        ("VI-VIM", Palette {
            bg: "#1c1c1c", sidebar: Some("#161616"), surface: "#262626", surface_active: Some("#303030"),
            text: "#cccccc", text_muted: Some("#808080"), accent: "#5f875f", border: Some("#303030"),
            terminal_bg: Some("#1c1c1c"), selection: Some("#5f5f00"),
            syntax: ["#87afd7", "#87af87", "#6c6c6c", "#d7af5f", "#5fafaf", "#d7d7af", "#cccccc", "#afafaf", "#949494", "#d75f5f"],
            ..P
        }),
        ("English Garden", Palette {
            bg: "#f0f4e8", sidebar: Some("#e5ecd6"), surface: "#ffffff", surface_active: Some("#dfe8cd"),
            text: "#2a3a1a", text_muted: Some("#5f6f4a"), accent: "#6a8a3a", danger: Some("#a3502f"),
            border: Some("#cdd9b5"), terminal_bg: Some("#f7faf0"), terminal_fg: Some("#2a3a1a"),
            selection: Some("#cfe0a8"),
            syntax: ["#7a2f6a", "#3a6b2a", "#5f6f4a", "#8a4b1a", "#1f5f6a", "#6a4a1a", "#2a3a1a", "#44563a", "#6b6152", "#a3231a"],
            ..P
        }),
        ("Matrix", Palette {
            bg: "#000000", sidebar: Some("#020402"), surface: "#031003", surface_active: Some("#052205"),
            text: "#00ff41", text_muted: Some("#0aa028"), accent: "#00ff41", border: Some("#0a3a0a"),
            terminal_bg: Some("#000000"), terminal_fg: Some("#00ff41"), selection: Some("#0f4f0f"),
            syntax: ["#7dff9a", "#00e07a", "#0a8a2a", "#b8ff6b", "#3affc0", "#c8ffd0", "#00ff41", "#4ade80", "#28a745", "#ffb000"],
            ..P
        }),
        ("Cyberpunk", Palette {
            bg: "#000000", sidebar: Some("#000000"), surface: "#1a0209", surface_active: Some("#2a0410"),
            text: "#f3e9ec", text_muted: Some("#b78a93"), accent: "#55ead4", danger: Some("#c5003c"),
            success: Some("#55ead4"), border: Some("#880425"), status_bar: Some("#000000"),
            terminal_bg: Some("#000000"), terminal_fg: Some("#f3e9ec"), editor_bg: Some("#000000"),
            editor_fg: Some("#f3e9ec"), selection: Some("#3a0212"), unsaved: Some("#f3e600"),
            syntax: ["#ff2e63", "#f3e600", "#a06070", "#ff9f1c", "#55ead4", "#7df9ff", "#f3e9ec", "#d0a8b4", "#b78a93", "#ff3860"],
            ..P
        }),
        ("Claude", Palette {
            bg: "#1a1613", sidebar: Some("#14100d"), surface: "#262019", surface_active: Some("#332a20"),
            text: "#e8e0d5", text_muted: Some("#a89b8a"), accent: "#d97757", danger: Some("#c15f3c"),
            border: Some("#332a20"), terminal_bg: Some("#161210"), terminal_fg: Some("#e8e0d5"),
            selection: Some("#3d3020"),
            syntax: ["#d97757", "#a3b18a", "#8c8074", "#e0b070", "#89b4b8", "#e8c07d", "#e8e0d5", "#bfb3a4", "#a89b8a", "#e05c4a"],
            ..P
        }),
        ("Debian", Palette {
            bg: "#1a1a1a", sidebar: Some("#141414"), surface: "#242424", surface_active: Some("#2f2020"),
            text: "#cccccc", text_muted: Some("#888888"), accent: "#d70a53", danger: Some("#d70a53"),
            border: Some("#2f2020"), terminal_bg: Some("#1a1a1a"), selection: Some("#4a1226"),
            syntax: ["#ee5396", "#a2c4a0", "#7a7a7a", "#e8a05c", "#79b8ca", "#d8b4dd", "#cccccc", "#b0b0b0", "#949494", "#ff5c5c"],
            ..P
        }),
        ("Ubuntu", Palette {
            bg: "#2c001e", sidebar: Some("#24001a"), surface: "#3d0a2c", surface_active: Some("#4d1339"),
            text: "#eeeeec", text_muted: Some("#b8a0b0"), accent: "#e95420", danger: Some("#e95420"),
            border: Some("#4d1339"), terminal_bg: Some("#300a24"), terminal_fg: Some("#eeeeec"),
            selection: Some("#5c1a44"),
            syntax: ["#e95420", "#aed581", "#a08090", "#f0c674", "#77c4d3", "#dfbde0", "#eeeeec", "#c8b0bc", "#b8a0b0", "#ff6f5e"],
            ..P
        }),
    ];
    for (name, palette) in &palettes {
        let mut theme = make(name, palette);
        if *name == "SUBNET" {
            // Neon Cyan, the brand's second accent, on the cursors.
            theme.set("terminalCursor", c("#00EFFF"));
            theme.set("editorCursor", c("#00EFFF"));
        }
        out.push(theme);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twenty_eight_distinct_builtins_with_every_token() {
        let themes = builtins();
        assert_eq!(themes.len(), 28);
        let names: std::collections::HashSet<String> = themes.iter().map(|t| t.name.to_lowercase()).collect();
        assert_eq!(names.len(), 28);
        for theme in &themes {
            for token in TOKENS {
                assert!(theme.colours.contains_key(token.key), "{} lacks {}", theme.name, token.key);
            }
        }
        assert!(themes[0].is_dark() && !resolve(&themes, "Light", true).is_dark());
    }

    #[test]
    fn code_reads_at_the_house_standard_on_the_editor_and_through_both_match_surfaces() {
        // 6.0:1 is throng's standard for code on the editor body (WCAG's 4.5:1 is the
        // floor), and code keeps 4.5:1 through the search highlights it is read on.
        for theme in builtins() {
            let bg = theme.colour("editorBg");
            for key in SYNTAX_KEYS {
                let colour = theme.colour(key);
                let on_body = contrast(colour, bg);
                assert!(on_body >= 6.0, "{} {key}: {on_body:.2}", theme.name);
                for surface in ["searchMatch", "searchMatchCurrent"] {
                    let ratio = contrast(colour, theme.colour(surface));
                    assert!(ratio >= 4.5, "{} {key} on {surface}: {ratio:.2}", theme.name);
                }
            }
            // The current match is the louder of the two.
            let (m, cur) = (theme.colour("searchMatch"), theme.colour("searchMatchCurrent"));
            assert!(delta_e(m, bg) <= delta_e(cur, bg) + 0.01, "{}", theme.name);
        }
    }

    #[test]
    fn text_reads_on_its_ground_and_terminal_colours_read_on_the_terminal() {
        let mut failed = Vec::new();
        for theme in builtins() {
            for (text, ground) in [
                ("text", "appBg"),
                ("text", "sidebarBg"),
                ("textMuted", "appBg"),
                ("textMuted", "sidebarBg"),
                ("editorFg", "editorBg"),
                ("terminalFg", "terminalBg"),
            ] {
                let ratio = contrast(theme.colour(text), theme.colour(ground));
                if ratio < 4.5 {
                    failed.push(format!("{} {text} on {ground}: {ratio:.2}", theme.name));
                }
            }
            let Some(ansi) = theme.ansi() else { continue };
            let ground = theme.colour("terminalBg");
            let dark = theme.is_dark();
            for (index, colour) in ansi.iter().enumerate() {
                let background = if dark { index == 0 } else { index == 7 || index == 15 };
                let min = if index == 8 { 3.0 } else { 4.5 };
                let ratio = contrast(*colour, ground);
                if !background && ratio < min {
                    failed.push(format!("{} ANSI {index}: {ratio:.2}", theme.name));
                }
            }
        }
        assert!(failed.is_empty(), "{failed:#?}");
    }

    #[test]
    fn a_themes_own_terminal_colours_survive_a_duplicate_and_a_round_trip() {
        let themes = builtins();
        let nord = resolve(&themes, "nord", true);
        let copy = nord.cloned_as("Mine");
        assert_eq!(copy.ansi(), nord.ansi());
        let again = Theme::parse(&copy.write_into(None), &themes[0]).unwrap();
        assert!(again.ansi().is_some() && again.ansi() == nord.ansi());
        let short = r##"{"name":"Short","ansi":["#000000"]}"##;
        assert_eq!(Theme::parse(short, &themes[0]).unwrap().ansi(), None, "all sixteen or none");
    }

    #[test]
    fn colour_difference_is_zero_for_a_colour_symmetric_and_100_from_black_to_white() {
        let grey = Colour { r: 128, g: 128, b: 128 };
        assert!(delta_e(grey, grey).abs() < 1e-9);
        let (black, white) = (Colour { r: 0, g: 0, b: 0 }, Colour { r: 255, g: 255, b: 255 });
        assert!((delta_e(black, white) - 100.0).abs() < 0.1, "{}", delta_e(black, white));
        assert!((delta_e(c("#ff0000"), c("#00ff00")) - delta_e(c("#00ff00"), c("#ff0000"))).abs() < 1e-9);
    }

    #[test]
    fn a_partial_theme_file_takes_the_rest_from_its_base_and_keeps_what_it_does_not_model() {
        let base = builtins().remove(0);
        let text =
            r##"{"name":"Mine","colours":{"accent":"#ff0000","nonsense":"#123456"},"fonts":{"family":"x"}}"##;
        let mine = Theme::parse(text, &base).unwrap();
        assert_eq!(mine.colour("accent"), c("#ff0000"));
        assert_eq!(mine.colour("editorBg"), base.colour("editorBg"));
        let written = mine.write_into(Some(text));
        assert!(written.contains("\"fonts\"") && written.contains("nonsense"), "{written}");
        assert!(Theme::parse("{}", &base).is_err());
        assert!(Theme::parse("not json", &base).is_err());
    }

    #[test]
    fn names_resolve_case_insensitively_and_old_values_map() {
        let themes = builtins();
        assert_eq!(resolve(&themes, "vscode", true).name, "VSCode");
        assert_eq!(resolve(&themes, "dark", false).name, "throng");
        assert_eq!(resolve(&themes, "light", true).name, "Light");
        assert_eq!(resolve(&themes, "system", false).name, "Light");
        assert_eq!(resolve(&themes, "nope", true).name, "throng");
        assert_eq!(file_name("A/B: C"), "A-B- C.json");
    }
}
