//! Icons: glyphs by token from the icon pack in force, and icons drawn as shapes for symbols the
//! bundled fonts do not have (a magnifier has no glyph in egui's fonts). Both keep the host
//! control's text colour, so themes colour them like text.

use std::path::Path;
use std::sync::Arc;

use egui::{Color32, Context, Id, Rect, Response, Sense, Stroke, Ui, pos2, vec2};
use throng_core::icons::{IconPack, IconSet};

fn set_id() -> Id {
    Id::new("throng-icons")
}

/// Make `set` the glyphs every icon is drawn with.
pub fn install(ctx: &Context, set: Arc<IconSet>) {
    ctx.data_mut(|d| d.insert_temp(set_id(), set));
}

/// The glyph for an icon token.
#[must_use]
pub fn glyph(ctx: &Context, token: &str) -> String {
    ctx.data(|d| d.get_temp::<Arc<IconSet>>(set_id()))
        .map_or_else(|| IconSet::default().get(token).to_owned(), |set| set.get(token).to_owned())
}

/// What draws the icon for `token`, for a button or label: the pack's image, sized to the text and
/// named for assistive technology, when it has one that loads; otherwise the glyph.
#[must_use]
pub fn atom(ctx: &Context, token: &str) -> egui::Atom<'static> {
    atom_with(ctx, token, |text| text)
}

/// [`atom`], with the glyph's text styled by `style` (an image is drawn as it is).
pub fn atom_with(
    ctx: &Context,
    token: &str,
    style: impl FnOnce(egui::RichText) -> egui::RichText,
) -> egui::Atom<'static> {
    let set = ctx.data(|d| d.get_temp::<Arc<IconSet>>(set_id()));
    if let Some(path) = set.as_ref().and_then(|s| s.image(token)) {
        let size = ctx.global_style().text_styles.get(&egui::TextStyle::Button).map_or(14.0, |f| f.size);
        let name = throng_core::icons::ICONS.iter().find(|i| i.token == token).map_or(token, |i| i.label);
        let image = egui::Image::new(crate::preview::file_uri(path))
            .fit_to_exact_size(vec2(size, size))
            .alt_text(name);
        // One that will not decode draws the glyph instead.
        if image.load_for_size(ctx, vec2(size, size)).is_ok() {
            return image.into();
        }
    }
    style(egui::RichText::new(glyph(ctx, token))).into()
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
    let response = ui.add_enabled(enabled, egui::Button::new(atom(ui.ctx(), token)).small());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, name));
    response
}

/// A small frameless button showing `icon`, named for assistive technology.
pub fn button(ui: &mut Ui, icon: Icon, name: &str) -> Response {
    let size = ui.spacing().interact_size.y;
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let visuals = ui.style().interact(&response);
    if response.hovered() {
        ui.painter().rect_filled(rect, 3.0, visuals.weak_bg_fill);
    }
    paint(ui, icon, rect.shrink(2.0), visuals.fg_stroke.color);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
    let _ = pos2(0.0, 0.0);
    response
}
