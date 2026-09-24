//! Icon packs: the icons the interface draws, by token, as glyphs or images. A pack lives in
//! `<config>/icon-packs/<name>/pack.json` as
//! `{ "name": …, "tokens": { "folder": "📁", "file": { "glyph": "📄" }, "refresh": "spin.svg" } }`.
//! A token a pack does not set, or sets to a glyph the fonts cannot draw or an image that cannot be
//! used, keeps throng's own glyph: a half-finished pack never leaves a hole in the interface.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// An icon the interface draws, and throng's own glyph for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IconDef {
    pub token: &'static str,
    pub label: &'static str,
    pub glyph: &'static str,
}

const fn icon(token: &'static str, label: &'static str, glyph: &'static str) -> IconDef {
    IconDef { token, label, glyph }
}

/// Every icon a pack can change.
pub const ICONS: &[IconDef] = &[
    icon("folder", "Folder", "🗀"),
    icon("folderOpen", "Open folder", "🗁"),
    icon("file", "File", "🗋"),
    icon("chevron", "Collapsed", "⏵"),
    icon("chevronOpen", "Expanded", "⏷"),
    icon("newFile", "New file", "🗋"),
    icon("newFolder", "New folder", "🗀"),
    icon("refresh", "Refresh", "⟳"),
    icon("add", "Add", "+"),
    icon("dismiss", "Dismiss", "🗙"),
    icon("retry", "Reset to default", "↺"),
    icon("findNext", "Next match", "⏷"),
    icon("findPrevious", "Previous match", "⏶"),
    icon("back", "Back", "⏴"),
    icon("forward", "Forward", "⏵"),
];

/// A pack as its file describes it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IconPack {
    /// The pack's folder name, which is what `appearance.iconPack` names.
    pub name: String,
    /// The name the pack gives itself, if any.
    pub title: Option<String>,
    glyphs: BTreeMap<String, String>,
    /// Tokens the pack sets to an image, and the image's file as written (relative to the pack).
    pub images: BTreeMap<String, String>,
}

impl IconPack {
    /// Read `pack.json`. Tolerant: malformed tokens are dropped rather than failing the pack.
    pub fn parse(folder: &str, text: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON ({e})"))?;
        let object = value.as_object().ok_or("not a JSON object")?;
        let mut pack = IconPack {
            name: folder.to_owned(),
            title: object.get("name").and_then(Value::as_str).map(str::to_owned),
            ..IconPack::default()
        };
        let Some(tokens) = object.get("tokens").and_then(Value::as_object) else { return Ok(pack) };
        for (token, raw) in tokens {
            let (glyph, image) = match raw {
                Value::String(s) if is_image(s) => (None, Some(s.clone())),
                Value::String(s) => (Some(s.clone()), None),
                Value::Object(o) => {
                    let image = o.get("image").and_then(Value::as_str).filter(|s| !s.trim().is_empty());
                    (o.get("glyph").and_then(Value::as_str).map(str::to_owned), image.map(str::to_owned))
                }
                _ => (None, None),
            };
            // An image wins; a glyph beside it is what shows if the image cannot be used.
            if let Some(image) = image {
                pack.images.insert(token.clone(), image);
            }
            if let Some(glyph) = glyph.filter(|g| !g.trim().is_empty()) {
                pack.glyphs.insert(token.clone(), glyph);
            }
        }
        Ok(pack)
    }

    #[must_use]
    pub fn glyph(&self, token: &str) -> Option<&str> {
        self.glyphs.get(token).map(String::as_str)
    }
}

fn is_image(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.ends_with(".svg") || lower.ends_with(".png")
}

/// What each icon draws: a pack's image where it has one that can be used, else its glyph where
/// the fonts can draw it, else throng's glyph. The glyph is kept beside an image, for anywhere
/// the image cannot load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IconSet {
    glyphs: BTreeMap<&'static str, String>,
    images: BTreeMap<&'static str, PathBuf>,
    /// Tokens whose glyph the pack chose: they draw as that glyph, never as throng's own image.
    chosen: std::collections::BTreeSet<&'static str>,
}

impl Default for IconSet {
    fn default() -> Self {
        Self {
            glyphs: ICONS.iter().map(|i| (i.token, i.glyph.to_owned())).collect(),
            images: BTreeMap::new(),
            chosen: std::collections::BTreeSet::new(),
        }
    }
}

impl IconSet {
    /// The set `pack` makes. `drawable` says whether the fonts can draw a glyph; `image` finds the
    /// file an image token names, or `None` when it cannot be used. Also returns the tokens the
    /// pack set that kept throng's glyph because what it gave could not be used.
    #[must_use]
    pub fn with_pack(
        pack: &IconPack,
        drawable: impl Fn(&str) -> bool,
        image: impl Fn(&str) -> Option<PathBuf>,
    ) -> (Self, Vec<String>) {
        let mut set = Self::default();
        let mut kept = Vec::new();
        for def in ICONS {
            let glyph = pack.glyph(def.token);
            if let Some(glyph) = glyph.filter(|g| drawable(g)) {
                set.glyphs.insert(def.token, glyph.to_owned());
                set.chosen.insert(def.token);
            }
            let file = pack.images.get(def.token).and_then(|written| image(written));
            let has_image = file.is_some();
            if let Some(file) = file {
                set.images.insert(def.token, file);
            }
            let asked = glyph.is_some() || pack.images.contains_key(def.token);
            let got = has_image || glyph.is_some_and(&drawable);
            if asked && !got {
                kept.push(def.token.to_owned());
            }
        }
        (set, kept)
    }

    /// The glyph for `token` (an unknown token draws nothing).
    #[must_use]
    pub fn get(&self, token: &str) -> &str {
        self.glyphs.get(token).map_or("", String::as_str)
    }

    /// The image file for `token`, when the pack gives one.
    #[must_use]
    pub fn image(&self, token: &str) -> Option<&Path> {
        self.images.get(token).map(PathBuf::as_path)
    }

    /// Whether the pack chose this token's icon (an image or a glyph), rather than leaving it to
    /// throng's own.
    #[must_use]
    pub fn chosen(&self, token: &str) -> bool {
        self.chosen.contains(token) || self.images.contains_key(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pack_reads_glyphs_both_ways_and_notes_images_and_drops_the_malformed() {
        let text = r#"{"name":"Mine","tokens":{
            "folder":"F","file":{"glyph":"D"},"refresh":"spin.svg","dismiss":{"image":"x.png"},
            "add":42,"retry":"","unknownToken":"U"}}"#;
        let pack = IconPack::parse("mine", text).unwrap();
        assert_eq!((pack.name.as_str(), pack.title.as_deref()), ("mine", Some("Mine")));
        assert_eq!(pack.glyph("folder"), Some("F"));
        assert_eq!(pack.glyph("file"), Some("D"));
        assert_eq!(pack.glyph("add"), None);
        assert_eq!(pack.glyph("retry"), None, "an empty glyph is no glyph");
        assert_eq!(
            pack.images.iter().map(|(t, f)| (t.as_str(), f.as_str())).collect::<Vec<_>>(),
            [("dismiss", "x.png"), ("refresh", "spin.svg")]
        );
        assert!(IconPack::parse("x", "[").is_err() && IconPack::parse("x", "[]").is_err());
        assert!(IconPack::parse("x", "{}").is_ok(), "a pack of nothing is throng's own");
    }

    #[test]
    fn a_glyph_the_fonts_cannot_draw_or_an_image_that_cannot_be_used_keeps_throngs_and_says_so() {
        let text = r#"{"tokens":{"folder":"F","file":"?","refresh":"r.svg","add":"gone.svg",
            "dismiss":{"image":"x.svg","glyph":"X"}}}"#;
        let pack = IconPack::parse("p", text).unwrap();
        let usable = |written: &str| (written != "gone.svg").then(|| PathBuf::from("/packs/p").join(written));
        let (set, kept) = IconSet::with_pack(&pack, |g| g != "?", usable);
        assert_eq!(set.get("folder"), "F");
        assert_eq!(set.get("file"), "🗋");
        assert_eq!(set.get("chevron"), "⏵");
        assert_eq!(set.image("refresh"), Some(Path::new("/packs/p/r.svg")));
        assert_eq!((set.image("dismiss"), set.get("dismiss")), (Some(Path::new("/packs/p/x.svg")), "X"));
        assert_eq!((set.image("add"), set.get("add")), (None, "+"), "a missing image keeps throng's");
        assert_eq!(kept, ["file", "add"]);
        assert!(set.chosen("folder") && set.chosen("refresh"), "a glyph or an image the pack chose");
        assert!(!set.chosen("file") && !set.chosen("chevron"), "throng's own where it chose nothing usable");
        assert_eq!(IconSet::default().get("nope"), "");
    }
}
