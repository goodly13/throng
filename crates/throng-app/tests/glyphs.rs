//! Every glyph the UI draws as text exists in the bundled fonts: a missing one renders as an empty
//! box, which no unit test would otherwise notice.

use std::collections::BTreeSet;
use std::path::Path;

/// Symbols drawn from places the source scan below cannot see (none today; keep for safety).
const EXTRA_GLYPHS: &[char] = &['+', '•', '⏵', '⏷', '⏶', '🗙'];

/// Every non-ASCII character inside a string or char literal in the app's source. Comments are
/// skipped; everything else a literal holds may end up on screen.
fn literal_glyphs(dir: &Path, out: &mut BTreeSet<char>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            literal_glyphs(&path, out);
            continue;
        }
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        for line in source.lines() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            let mut in_literal: Option<char> = None;
            let mut escaped = false;
            let mut previous = ' ';
            for c in code.chars() {
                match in_literal {
                    Some(quote) => {
                        if escaped {
                            escaped = false;
                        } else if c == '\\' {
                            escaped = true;
                        } else if c == quote {
                            in_literal = None;
                        } else if !c.is_ascii() {
                            out.insert(c);
                        }
                    }
                    None => {
                        if c == '/' && previous == '/' {
                            break;
                        }
                        // A `'` after an identifier character is a lifetime, not a char literal.
                        if c == '"'
                            || (c == '\''
                                && !(previous.is_alphanumeric() || previous == '_' || previous == '&'))
                        {
                            in_literal = Some(c);
                        }
                    }
                }
                previous = c;
            }
        }
    }
}

#[test]
fn every_ui_glyph_is_in_the_proportional_font() {
    let mut glyphs: BTreeSet<char> = EXTRA_GLYPHS.iter().copied().collect();
    // throng's own icon glyphs, which the app draws by token.
    glyphs.extend(throng_core::icons::ICONS.iter().flat_map(|i| i.glyph.chars()));
    literal_glyphs(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut glyphs);
    assert!(glyphs.contains(&'🗙') && glyphs.contains(&'›'), "the scan finds the UI's symbols: {glyphs:?}");
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
    output.textures_delta.clear();
    let font = egui::FontId::proportional(14.0);
    let missing: Vec<char> =
        glyphs.iter().copied().filter(|c| !ctx.fonts_mut(|f| f.has_glyph(&font, *c))).collect();
    let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
    output.textures_delta.clear();
    assert!(missing.is_empty(), "glyphs that would render as boxes: {missing:?}");
}
