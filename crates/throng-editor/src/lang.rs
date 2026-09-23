//! Which syntax a file gets: from its name only, the longest matching suffix winning,
//! with the user's own extension mappings first.

use std::collections::BTreeMap;

use syntect::parsing::SyntaxReference;

use crate::highlight::syntax_set;

/// Suffixes of a file name to try, longest first: `a.test.ts` → `a.test.ts`, `test.ts`, `ts`.
fn suffixes(file_name: &str) -> Vec<&str> {
    let mut out = vec![file_name];
    for (i, c) in file_name.char_indices() {
        if c == '.' && i + 1 < file_name.len() {
            out.push(&file_name[i + 1..]);
        }
    }
    out
}

/// The syntax for a file name, honouring `remap` (suffix → language name).
#[must_use]
pub fn detect(file_name: &str, remap: &BTreeMap<String, String>) -> &'static SyntaxReference {
    let set = syntax_set();
    for suffix in suffixes(file_name) {
        let lower = suffix.to_lowercase();
        if let Some(name) = remap.get(&lower).or_else(|| remap.get(suffix))
            && let Some(syntax) = by_name(name)
        {
            return syntax;
        }
        if let Some(syntax) =
            set.find_syntax_by_extension(suffix).or_else(|| set.find_syntax_by_extension(&lower))
        {
            return syntax;
        }
    }
    set.find_syntax_plain_text()
}

/// The syntax for a file name with no remapping.
#[must_use]
pub fn syntax_for_name(file_name: &str) -> &'static SyntaxReference {
    detect(file_name, &BTreeMap::new())
}

/// A syntax by its display name ("Rust", "Plain Text"), case-insensitively.
#[must_use]
pub fn by_name(name: &str) -> Option<&'static SyntaxReference> {
    let set = syntax_set();
    set.find_syntax_by_name(name)
        .or_else(|| set.syntaxes().iter().find(|s| s.name.eq_ignore_ascii_case(name)))
}

/// Every language, by name, for the language picker.
#[must_use]
pub fn names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> =
        syntax_set().syntaxes().iter().filter(|s| !s.hidden).map(|s| s.name.as_str()).collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names
}

#[must_use]
pub fn plain_text() -> &'static SyntaxReference {
    syntax_set().find_syntax_plain_text()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_extensions_map_to_languages() {
        for (name, language) in [
            ("main.rs", "Rust"),
            ("Cargo.toml", "TOML"),
            ("app.tsx", "TypeScriptReact"),
            ("Dockerfile", "Dockerfile"),
            ("Makefile", "Makefile"),
            ("README.md", "Markdown"),
            ("notes.unknownext", "Plain Text"),
        ] {
            assert_eq!(syntax_for_name(name).name, language, "{name}");
        }
    }

    #[test]
    fn the_longest_suffix_wins_and_remaps_come_first() {
        let mut remap = BTreeMap::new();
        remap.insert("conf".to_owned(), "INI".to_owned());
        assert_eq!(detect("app.conf", &remap).name, "INI");
        assert_eq!(suffixes("a.test.ts"), vec!["a.test.ts", "test.ts", "ts"]);
    }
}
