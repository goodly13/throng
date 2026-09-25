//! The interface's and the code's typefaces (`appearance.interfaceFont`, `appearance.codeFont`).
//! throng ships a few, so every choice draws offline; any installed family can be named too. The
//! bundled defaults and egui's own fonts always follow the chosen one, so a glyph the chosen font
//! lacks still draws (FR-044), and a family that cannot be found or read falls back to the default.

use std::sync::Arc;

use egui::{Context, FontData, FontDefinitions, FontFamily};

/// The family section titles and headings are set in: the interface font's semibold weight.
pub const HEADING: &str = "heading";

/// A typeface throng ships.
struct Bundled {
    family: &'static str,
    regular: &'static [u8],
    /// A heavier weight for headings, when the family ships one.
    semibold: Option<&'static [u8]>,
}

const INTER: Bundled = Bundled {
    family: "Inter",
    regular: include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    semibold: Some(include_bytes!("../assets/fonts/Inter-SemiBold.ttf")),
};
const GEIST: Bundled = Bundled {
    family: "Geist",
    regular: include_bytes!("../assets/fonts/Geist-Regular.ttf"),
    semibold: Some(include_bytes!("../assets/fonts/Geist-SemiBold.ttf")),
};
const JETBRAINS_MONO: Bundled = Bundled {
    family: "JetBrains Mono",
    regular: include_bytes!("../assets/fonts/JetBrainsMonoNL-Regular.ttf"),
    semibold: None,
};
const FIRA_CODE: Bundled = Bundled {
    family: "Fira Code",
    regular: include_bytes!("../assets/fonts/FiraCode-Regular.ttf"),
    semibold: None,
};

const INTERFACE_FONTS: [&Bundled; 2] = [&INTER, &GEIST];
const CODE_FONTS: [&Bundled; 2] = [&JETBRAINS_MONO, &FIRA_CODE];
/// egui's own monospaced font, which it always loads under this name.
const HACK: &str = "Hack";

/// The interface fonts throng ships, the default first.
#[must_use]
pub fn interface_fonts() -> Vec<&'static str> {
    INTERFACE_FONTS.iter().map(|b| b.family).collect()
}

/// The code fonts throng ships, the default first.
#[must_use]
pub fn code_fonts() -> Vec<&'static str> {
    CODE_FONTS.iter().map(|b| b.family).chain(std::iter::once(HACK)).collect()
}

/// The fonts installed on this machine, found once, when first asked for.
#[derive(Default)]
pub struct SystemFonts {
    db: Option<fontdb::Database>,
}

impl SystemFonts {
    fn db(&mut self) -> &fontdb::Database {
        self.db.get_or_insert_with(|| {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            db
        })
    }

    /// Every installed family's name, sorted; with `monospaced`, only fixed-width ones.
    pub fn families(&mut self, monospaced: bool) -> Vec<String> {
        let mut names: Vec<String> = self
            .db()
            .faces()
            .filter(|f| !monospaced || f.monospaced)
            .filter_map(|f| f.families.first().map(|(name, _)| name.clone()))
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        names
    }

    /// An installed family's face nearest `weight`, if it exists and can be read.
    fn load(&mut self, family: &str, weight: u16) -> Option<FontData> {
        let db = self.db();
        let query = fontdb::Query {
            families: &[fontdb::Family::Name(family)],
            weight: fontdb::Weight(weight),
            ..fontdb::Query::default()
        };
        let id = db.query(&query)?;
        db.with_face_data(id, |data, index| {
            readable(data, index).then(|| {
                let mut font = FontData::from_owned(data.to_vec());
                font.index = index;
                font
            })
        })
        .flatten()
    }
}

/// Whether egui can draw from this font: it parses and maps ordinary letters.
fn readable(data: &[u8], index: u32) -> bool {
    use skrifa::MetadataProvider as _;
    skrifa::FontRef::from_index(data, index).is_ok_and(|f| f.charmap().map('a').is_some())
}

fn bundled<'a>(list: &[&'a Bundled], name: &str) -> Option<&'a Bundled> {
    list.iter().copied().find(|b| b.family.eq_ignore_ascii_case(name.trim()))
}

/// A font the settings name that could not be used: which setting, and the name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Missing {
    pub label: &'static str,
    pub name: String,
    pub fallback: &'static str,
}

/// Give egui the fonts the settings name. Returns the ones that could not be found or read,
/// which draw in the default instead.
pub fn install(ctx: &Context, interface: &str, code: &str, system: &mut SystemFonts) -> Vec<Missing> {
    let (fonts, missing) = definitions(interface, code, system);
    ctx.set_fonts(fonts);
    missing
}

fn put(fonts: &mut FontDefinitions, key: &str, data: FontData) {
    fonts.font_data.insert(key.to_owned(), Arc::new(data));
}

/// The chain for a family: `first` (if any), then the fallbacks, each once.
fn chain(first: &[&str], fallbacks: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in first.iter().map(|s| (*s).to_owned()).chain(fallbacks.iter().cloned()) {
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// The font definitions for an interface and a code font.
#[must_use]
pub fn definitions(interface: &str, code: &str, system: &mut SystemFonts) -> (FontDefinitions, Vec<Missing>) {
    let mut fonts = FontDefinitions::default();
    let mut missing = Vec::new();
    let egui_proportional = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let egui_monospace = fonts.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    for b in INTERFACE_FONTS.iter().chain(CODE_FONTS.iter()) {
        put(&mut fonts, b.family, FontData::from_static(b.regular));
        if let Some(semibold) = b.semibold {
            put(&mut fonts, &format!("{} SemiBold", b.family), FontData::from_static(semibold));
        }
    }

    // The interface: a bundled family, else an installed one, else Inter.
    let (body, heading) = match bundled(&INTERFACE_FONTS, interface) {
        Some(b) => (b.family.to_owned(), format!("{} SemiBold", b.family)),
        None => match system.load(interface, 400) {
            Some(regular) => {
                put(&mut fonts, "interface", regular);
                let heading = match system.load(interface, 600) {
                    Some(bold) => {
                        put(&mut fonts, "interface heading", bold);
                        "interface heading"
                    }
                    None => "interface",
                };
                ("interface".to_owned(), heading.to_owned())
            }
            None => {
                missing.push(Missing {
                    label: "interface",
                    name: interface.trim().to_owned(),
                    fallback: INTER.family,
                });
                (INTER.family.to_owned(), format!("{} SemiBold", INTER.family))
            }
        },
    };
    let inter_semibold = format!("{} SemiBold", INTER.family);
    fonts.families.insert(FontFamily::Proportional, chain(&[&body, INTER.family], &egui_proportional));
    fonts.families.insert(
        FontFamily::Name(HEADING.into()),
        chain(&[&heading, &inter_semibold, INTER.family], &egui_proportional),
    );

    // The code: a bundled family (egui's Hack included), else an installed one, else the default.
    let code_family = if code.trim().eq_ignore_ascii_case(HACK) {
        HACK.to_owned()
    } else if let Some(b) = bundled(&CODE_FONTS, code) {
        b.family.to_owned()
    } else if let Some(data) = system.load(code, 400) {
        put(&mut fonts, "code", data);
        "code".to_owned()
    } else {
        missing.push(Missing {
            label: "code",
            name: code.trim().to_owned(),
            fallback: JETBRAINS_MONO.family,
        });
        JETBRAINS_MONO.family.to_owned()
    };
    // After the monospaced fonts, the interface's: a symbol no code font has still draws.
    let monospace_fallbacks: Vec<String> = egui_monospace
        .iter()
        .cloned()
        .chain(std::iter::once(INTER.family.to_owned()))
        .chain(egui_proportional)
        .collect();
    fonts.families.insert(
        FontFamily::Monospace,
        chain(&[&code_family, JETBRAINS_MONO.family, HACK], &monospace_fallbacks),
    );
    (fonts, missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn family(fonts: &FontDefinitions, f: FontFamily) -> Vec<String> {
        fonts.families.get(&f).cloned().unwrap_or_default()
    }

    #[test]
    fn a_bundled_choice_leads_and_the_defaults_and_eguis_fonts_follow() {
        let mut system = SystemFonts::default();
        let (fonts, missing) = definitions("geist", "Fira Code", &mut system);
        assert!(missing.is_empty());
        let body = family(&fonts, FontFamily::Proportional);
        assert_eq!(body[..2], ["Geist".to_owned(), "Inter".to_owned()]);
        assert!(body.len() > 2, "egui's own fonts are still there for symbols and emoji");
        assert_eq!(family(&fonts, FontFamily::Name(HEADING.into()))[0], "Geist SemiBold");
        let mono = family(&fonts, FontFamily::Monospace);
        assert_eq!(mono[..3], ["Fira Code".to_owned(), "JetBrains Mono".to_owned(), HACK.to_owned()]);
        assert!(system.db.is_none(), "a bundled font never scans the system");
        let (fonts, _) = definitions("Inter", "hack", &mut system);
        assert_eq!(family(&fonts, FontFamily::Monospace)[0], HACK);
    }

    #[test]
    fn every_bundled_font_is_readable() {
        for b in INTERFACE_FONTS.iter().chain(CODE_FONTS.iter()) {
            assert!(readable(b.regular, 0), "{}", b.family);
            assert!(b.semibold.is_none_or(|s| readable(s, 0)), "{}", b.family);
        }
        assert!(!readable(b"not a font", 0));
    }

    #[test]
    fn a_family_that_is_not_installed_falls_back_and_says_so() {
        let mut system = SystemFonts::default();
        let (fonts, missing) = definitions("No Such Family 7f3a", "No Such Mono 7f3a", &mut system);
        assert_eq!(missing.len(), 2);
        assert_eq!((missing[0].label, missing[0].fallback), ("interface", "Inter"));
        assert_eq!((missing[1].label, missing[1].fallback), ("code", "JetBrains Mono"));
        assert_eq!(family(&fonts, FontFamily::Proportional)[0], "Inter");
        assert_eq!(family(&fonts, FontFamily::Monospace)[0], "JetBrains Mono");
    }
}
