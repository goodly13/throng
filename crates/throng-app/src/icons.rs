//! Icons: throng's own are Lucide's (ISC), bundled as SVGs and drawn in the text's colour. An icon
//! pack can replace any of them with a glyph or an image of its own, and a token with no image
//! falls back to its glyph. The painted icons are for symbols neither has.

use std::path::Path;
use std::sync::Arc;

use egui::{Color32, Context, Id, Rect, Response, Sense, Stroke, Ui, vec2};
use throng_core::icons::{IconPack, IconSet};

fn set_id() -> Id {
    Id::new("throng-icons")
}

/// throng's own icons, by token: Lucide's, drawn white so a tint gives them any colour.
const LUCIDE: &[(&str, &[u8])] = &[
    ("folder", include_bytes!("../assets/lucide/folder.svg")),
    ("folderOpen", include_bytes!("../assets/lucide/folder-open.svg")),
    ("file", include_bytes!("../assets/lucide/file.svg")),
    ("chevron", include_bytes!("../assets/lucide/chevron-right.svg")),
    ("chevronOpen", include_bytes!("../assets/lucide/chevron-down.svg")),
    ("newFile", include_bytes!("../assets/lucide/file-plus.svg")),
    ("newFolder", include_bytes!("../assets/lucide/folder-plus.svg")),
    ("refresh", include_bytes!("../assets/lucide/refresh-cw.svg")),
    ("add", include_bytes!("../assets/lucide/plus.svg")),
    ("dismiss", include_bytes!("../assets/lucide/x.svg")),
    ("retry", include_bytes!("../assets/lucide/rotate-ccw.svg")),
    ("findNext", include_bytes!("../assets/lucide/arrow-down.svg")),
    ("findPrevious", include_bytes!("../assets/lucide/arrow-up.svg")),
    ("back", include_bytes!("../assets/lucide/arrow-left.svg")),
    ("forward", include_bytes!("../assets/lucide/arrow-right.svg")),
    // Not a pack's to change: kinds of file, and symbols with no glyph.
    ("search", include_bytes!("../assets/lucide/search.svg")),
    ("terminal", include_bytes!("../assets/lucide/square-terminal.svg")),
    ("preview", include_bytes!("../assets/lucide/eye.svg")),
    ("fileCode", include_bytes!("../assets/lucide/file-code.svg")),
    ("fileJson", include_bytes!("../assets/lucide/file-json.svg")),
    ("fileText", include_bytes!("../assets/lucide/file-text.svg")),
    ("fileImage", include_bytes!("../assets/lucide/file-image.svg")),
    ("fileTerminal", include_bytes!("../assets/lucide/file-terminal.svg")),
    ("fileCog", include_bytes!("../assets/lucide/file-cog.svg")),
    ("fileArchive", include_bytes!("../assets/lucide/file-archive.svg")),
    ("fileSpreadsheet", include_bytes!("../assets/lucide/file-spreadsheet.svg")),
    ("fileLock", include_bytes!("../assets/lucide/file-lock.svg")),
];

fn lucide_uri(token: &str) -> Option<String> {
    LUCIDE.iter().any(|(t, _)| *t == token).then(|| format!("bytes://throng/icons/{token}.svg"))
}

/// Make `set` the glyphs every icon is drawn with, and throng's own icons loadable.
pub fn install(ctx: &Context, set: Arc<IconSet>) {
    ctx.data_mut(|d| d.insert_temp(set_id(), set));
    let installed = Id::new("throng-icons-installed");
    if !ctx.data(|d| d.get_temp::<bool>(installed).unwrap_or(false)) {
        for (token, svg) in LUCIDE {
            // Their strokes are `currentColor`; white, so the tint alone decides the colour.
            let white = String::from_utf8_lossy(svg).replace("currentColor", "#ffffff");
            ctx.include_bytes(format!("bytes://throng/icons/{token}.svg"), white.into_bytes());
        }
        ctx.data_mut(|d| d.insert_temp(installed, true));
    }
}

/// The glyph for an icon token.
#[must_use]
pub fn glyph(ctx: &Context, token: &str) -> String {
    ctx.data(|d| d.get_temp::<Arc<IconSet>>(set_id()))
        .map_or_else(|| IconSet::default().get(token).to_owned(), |set| set.get(token).to_owned())
}

/// Which of the text's colours an icon takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Text,
    Weak,
    Strong,
}

impl Tone {
    fn colour(self, visuals: &egui::Visuals) -> Color32 {
        match self {
            Self::Text => visuals.text_color(),
            Self::Weak => visuals.weak_text_color(),
            Self::Strong => visuals.strong_text_color(),
        }
    }

    fn style(self, text: egui::RichText) -> egui::RichText {
        match self {
            Self::Text => text,
            Self::Weak => text.weak(),
            Self::Strong => text.strong(),
        }
    }
}

/// The size an icon is drawn at: the height of a button's text.
fn icon_size(ctx: &Context) -> f32 {
    ctx.global_style().text_styles.get(&egui::TextStyle::Button).map_or(14.0, |f| f.size)
}

/// throng's own image for `token` in `colour`, when it has one.
#[must_use]
pub fn own_image(ctx: &Context, token: &str, colour: Color32) -> Option<egui::Image<'static>> {
    let size = icon_size(ctx);
    let name = throng_core::icons::ICONS.iter().find(|i| i.token == token).map_or(token, |i| i.label);
    Some(egui::Image::new(lucide_uri(token)?).fit_to_exact_size(vec2(size, size)).tint(colour).alt_text(name))
}

/// The icon for a kind of thing (`kind`, such as `fileJson`), in `colour`, standing in for the
/// pack token `token` (`file`): a pack that chose that token's icon draws it for every kind.
#[must_use]
pub fn kind_atom(ctx: &Context, kind: &str, token: &str, colour: Color32) -> egui::Atom<'static> {
    let chosen = ctx.data(|d| d.get_temp::<Arc<IconSet>>(set_id())).is_some_and(|s| s.chosen(token));
    if !chosen && let Some(image) = own_image(ctx, kind, colour) {
        return image.into();
    }
    atom(ctx, token)
}

/// What draws the icon for `token`, for a button or label: the pack's image or glyph when it chose
/// one, else throng's own icon, else the glyph.
#[must_use]
pub fn atom(ctx: &Context, token: &str) -> egui::Atom<'static> {
    atom_with(ctx, token, Tone::Text)
}

/// [`atom`] in one of the text's colours.
#[must_use]
pub fn atom_with(ctx: &Context, token: &str, tone: Tone) -> egui::Atom<'static> {
    let set = ctx.data(|d| d.get_temp::<Arc<IconSet>>(set_id()));
    if let Some(path) = set.as_ref().and_then(|s| s.image(token)) {
        let size = icon_size(ctx);
        let name = throng_core::icons::ICONS.iter().find(|i| i.token == token).map_or(token, |i| i.label);
        let image = egui::Image::new(crate::preview::file_uri(path))
            .fit_to_exact_size(vec2(size, size))
            .alt_text(name);
        // One that will not decode draws the glyph instead.
        if image.load_for_size(ctx, vec2(size, size)).is_ok() {
            return image.into();
        }
    }
    let chosen = set.as_ref().is_some_and(|s| s.chosen(token));
    if !chosen && let Some(image) = own_image(ctx, token, tone.colour(&ctx.global_style().visuals)) {
        return image.into();
    }
    tone.style(egui::RichText::new(glyph(ctx, token))).into()
}

/// The largest image an icon pack may use.
const MAX_ICON_BYTES: u64 = 1024 * 1024;

/// The file an icon pack's image token names, when it can be used: an SVG or PNG inside the pack's
/// folder (links followed), no larger than a mebibyte.
#[must_use]
pub fn pack_image(folder: &Path, written: &str) -> Option<std::path::PathBuf> {
    let lower = written.to_ascii_lowercase();
    if !(lower.ends_with(".svg") || lower.ends_with(".png")) || Path::new(written).is_absolute() {
        return None;
    }
    let folder = std::fs::canonicalize(folder).ok()?;
    let path = std::fs::canonicalize(folder.join(written)).ok()?;
    let meta = std::fs::metadata(&path).ok()?;
    (path.starts_with(&folder) && meta.is_file() && meta.len() <= MAX_ICON_BYTES).then_some(path)
}

/// Whether the interface font can draw every character of `glyph`.
#[must_use]
pub fn drawable(ctx: &Context, glyph: &str) -> bool {
    let font = egui::FontId::proportional(14.0);
    ctx.fonts_mut(|f| glyph.chars().filter(|c| !c.is_whitespace()).all(|c| f.has_glyph(&font, c)))
}

/// The packs in `dir` (each a folder holding `pack.json`), in name order, and the folders that
/// could not be read as one.
#[must_use]
pub fn scan_packs(dir: &Path) -> (Vec<IconPack>, Vec<(String, String)>) {
    let mut folders: Vec<_> = std::fs::read_dir(dir)
        .map(|d| d.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_dir()).collect())
        .unwrap_or_default();
    folders.sort();
    let mut packs = Vec::new();
    let mut problems = Vec::new();
    for folder in folders {
        let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let manifest = folder.join("pack.json");
        if !manifest.exists() {
            continue;
        }
        match std::fs::read_to_string(&manifest)
            .map_err(|e| e.to_string())
            .and_then(|t| IconPack::parse(&name, &t))
        {
            Ok(pack) => packs.push(pack),
            Err(reason) => problems.push((name, reason)),
        }
    }
    (packs, problems)
}

/// A drawn icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Search,
}

fn paint(ui: &Ui, icon: Icon, rect: Rect, colour: Color32) {
    let painter = ui.painter_at(rect.expand(1.0));
    let stroke = Stroke::new(1.5, colour);
    match icon {
        Icon::Search => {
            let r = rect.width().min(rect.height()) * 0.28;
            let centre = rect.center() - vec2(r * 0.35, r * 0.35);
            painter.circle_stroke(centre, r, stroke);
            let d = r * std::f32::consts::FRAC_1_SQRT_2;
            let from = centre + vec2(d, d);
            painter.line_segment([from, from + vec2(r * 0.9, r * 0.9)], Stroke::new(2.0, colour));
        }
    }
}

/// A small button showing the icon for `token`, named `name` for assistive technology and
/// enabled or not.
pub fn token_button(ui: &mut Ui, token: &str, name: &str, enabled: bool) -> Response {
    let button = egui::Button::new(atom(ui.ctx(), token)).small().frame_when_inactive(false);
    let response = ui.add_enabled(enabled, button);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, name));
    response
}

/// A small frameless button showing `icon`, named for assistive technology.
pub fn button(ui: &mut Ui, icon: Icon, name: &str) -> Response {
    let size = ui.spacing().interact_size.y;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let visuals = ui.style().interact(&response);
    if response.hovered() {
        ui.painter().rect_filled(rect, 4.0, visuals.weak_bg_fill);
    }
    let token = match icon {
        Icon::Search => "search",
    };
    let colour = visuals.fg_stroke.color;
    match own_image(ui.ctx(), token, colour) {
        Some(image) => {
            let side = icon_size(ui.ctx());
            image.paint_at(ui, Rect::from_center_size(rect.center(), vec2(side, side)));
        }
        None => paint(ui, icon, rect.shrink(2.0), colour),
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
    response
}
