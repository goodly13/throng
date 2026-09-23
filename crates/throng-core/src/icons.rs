//! Icon packs: glyphs for the icons the interface draws as text, by token. A pack lives in `<config>/icon-packs/<name>/pack.json`
//! as `{ "name": …, "tokens": { "folder": "📁", "file": { "glyph": "📄" } } }`. A token a pack does
//! not set, sets to an image, or sets to a glyph the fonts cannot draw keeps throng's own glyph:
//! a half-finished pack never leaves a hole in the interface.

use std::collections::BTreeMap;

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
];

/// A pack as its file describes it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IconPack {
    /// The pack's folder name, which is what `appearance.iconPack` names.
    pub name: String,
    /// The name the pack gives itself, if any.
    pub title: Option<String>,
    glyphs: BTreeMap<String, String>,
    /// Tokens the pack sets to an image, which this app does not draw.
    pub images: Vec<String>,
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
                Value::String(s) if is_image(s) => (None, true),
                Value::String(s) => (Some(s.clone()), false),
                Value::Object(o) => match (o.get("glyph").and_then(Value::as_str), o.get("image")) {
                    (Some(g), _) => (Some(g.to_owned()), false),
                    (None, Some(_)) => (None, true),
                    _ => (None, false),
                },
                _ => (None, false),
            };
            if let Some(glyph) = glyph.filter(|g| !g.trim().is_empty()) {
                pack.glyphs.insert(token.clone(), glyph);
            } else if image {
                pack.images.push(token.clone());
            }
        }
        pack.images.sort();
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

/// The glyph drawn for each icon: a pack's where it has one the fonts can draw, else throng's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IconSet {
    glyphs: BTreeMap<&'static str, String>,
}

impl Default for IconSet {
    fn default() -> Self {
        Self { glyphs: ICONS.iter().map(|i| (i.token, i.glyph.to_owned())).collect() }
    }
}

impl IconSet {
    /// The set `pack` makes, keeping only glyphs `drawable` accepts. Also returns the tokens that
    /// kept throng's glyph because the pack's could not be drawn (images included).
    #[must_use]
    pub fn with_pack(pack: &IconPack, drawable: impl Fn(&str) -> bool) -> (Self, Vec<String>) {
        let mut set = Self::default();
        let mut kept = Vec::new();
        for def in ICONS {
            match pack.glyph(def.token) {
                Some(glyph) if drawable(glyph) => {
                    set.glyphs.insert(def.token, glyph.to_owned());
                }
                Some(_) => kept.push(def.token.to_owned()),
                None if pack.images.iter().any(|t| t == def.token) => kept.push(def.token.to_owned()),
                None => {}
            }
        }
        (set, kept)
    }

    /// The glyph for `token` (an unknown token draws nothing).
    #[must_use]
    pub fn get(&self, token: &str) -> &str {
        self.glyphs.get(token).map_or("", String::as_str)
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
        assert_eq!(pack.images, ["dismiss", "refresh"]);
        assert!(IconPack::parse("x", "[").is_err() && IconPack::parse("x", "[]").is_err());
        assert!(IconPack::parse("x", "{}").is_ok(), "a pack of nothing is throng's own");
    }

    #[test]
    fn a_glyph_the_fonts_cannot_draw_keeps_throngs_and_says_so() {
        let pack = IconPack::parse("p", r#"{"tokens":{"folder":"F","file":"?","refresh":"r.svg"}}"#).unwrap();
        let (set, kept) = IconSet::with_pack(&pack, |g| g != "?");
        assert_eq!(set.get("folder"), "F");
        assert_eq!(set.get("file"), "🗋");
        assert_eq!(set.get("chevron"), "⏵");
        assert_eq!(kept, ["file", "refresh"]);
        assert_eq!(IconSet::default().get("nope"), "");
    }
}
