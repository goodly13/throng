//! The themes on offer: the fifteen built in, then the user's own from `<config>/themes/*.json`,
//! re-read whenever that folder changes. A user file is never written except through the theme
//! editor, and a write keeps whatever else the file holds.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use throng_core::theme::{self, Theme};

/// A theme file that could not be used, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unusable {
    pub file: String,
    pub reason: String,
}

pub struct ThemeStore {
    dir: PathBuf,
    themes: Vec<Theme>,
    /// Where each user theme came from, by lower-cased name.
    files: HashMap<String, PathBuf>,
    pub unusable: Vec<Unusable>,
}

impl ThemeStore {
    #[must_use]
    pub fn load(dir: PathBuf) -> Self {
        let mut store = Self { dir, themes: Vec::new(), files: HashMap::new(), unusable: Vec::new() };
        store.reload();
        store
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Read the folder again. Built-ins come first; user themes follow in name order.
    pub fn reload(&mut self) {
        let builtins = theme::builtins();
        let base = builtins[0].clone();
        let mut own = Vec::new();
        self.files.clear();
        self.unusable.clear();
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&self.dir)
            .map(|dir| {
                dir.filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")))
                    .collect()
            })
            .unwrap_or_default();
        entries.sort();
        for path in entries {
            let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let parsed = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|text| Theme::parse(&text, &base));
            let reason = match parsed {
                Ok(t) if builtins.iter().any(|b| b.name.eq_ignore_ascii_case(&t.name)) => {
                    format!("\"{}\" is the name of a built-in theme", t.name)
                }
                Ok(t) if self.files.contains_key(&t.name.to_lowercase()) => {
                    format!("another file already defines \"{}\"", t.name)
                }
                Ok(t) => {
                    self.files.insert(t.name.to_lowercase(), path.clone());
                    own.push(t);
                    continue;
                }
                Err(reason) => reason,
            };
            self.unusable.push(Unusable { file, reason });
        }
        own.sort_by_key(|t| t.name.to_lowercase());
        self.themes = builtins;
        self.themes.extend(own);
    }

    #[must_use]
    pub fn all(&self) -> &[Theme] {
        &self.themes
    }

    /// The theme a setting names, else `throng`.
    #[must_use]
    pub fn resolve(&self, name: &str, system_dark: bool) -> &Theme {
        theme::resolve(&self.themes, name, system_dark)
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Theme> {
        self.themes.iter().find(|t| t.name.eq_ignore_ascii_case(name))
    }

    /// A name no theme has yet: `base`, else `base 2`, `base 3`…
    #[must_use]
    pub fn unused_name(&self, base: &str) -> String {
        let base = base.trim();
        (1..)
            .map(|n| if n == 1 { base.to_owned() } else { format!("{base} {n}") })
            .find(|name| self.get(name).is_none() && !self.dir.join(theme::file_name(name)).exists())
            .unwrap_or_default()
    }

    /// Write a user theme to its file (creating the folder), keeping what the file holds besides.
    pub fn save(&mut self, theme: &Theme) -> Result<(), String> {
        if theme.builtin {
            return Err(format!("\"{}\" is built in; duplicate it to change it.", theme.name));
        }
        let path = self
            .files
            .get(&theme.name.to_lowercase())
            .cloned()
            .unwrap_or_else(|| self.dir.join(theme::file_name(&theme.name)));
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let existing = std::fs::read_to_string(&path).ok();
        throng_platform::fs::atomic_write(&path, theme.write_into(existing.as_deref()).as_bytes())
            .map_err(|e| e.to_string())?;
        self.files.insert(theme.name.to_lowercase(), path);
        match self.themes.iter_mut().find(|t| t.name.eq_ignore_ascii_case(&theme.name)) {
            Some(slot) => *slot = theme.clone(),
            None => {
                self.themes.push(theme.clone());
                let (builtin, mut own): (Vec<Theme>, Vec<Theme>) =
                    std::mem::take(&mut self.themes).into_iter().partition(|t| t.builtin);
                own.sort_by_key(|t| t.name.to_lowercase());
                self.themes = builtin;
                self.themes.extend(own);
            }
        }
        Ok(())
    }

    /// Change a user theme in memory only (the theme editor's live preview; [`Self::save`] writes it).
    pub fn update(&mut self, theme: Theme) {
        if let Some(slot) =
            self.themes.iter_mut().find(|t| !t.builtin && t.name.eq_ignore_ascii_case(&theme.name))
        {
            *slot = theme;
        }
    }

    /// Move a user theme's file to the trash, from where the platform can put it back.
    pub fn delete(&mut self, name: &str) -> Result<(), String> {
        let Some(path) = self.files.get(&name.to_lowercase()).cloned() else {
            return Err(format!("\"{name}\" is built in and cannot be deleted."));
        };
        throng_platform::fs::trash_restorable(&path)?;
        self.files.remove(&name.to_lowercase());
        self.themes.retain(|t| !t.name.eq_ignore_ascii_case(name));
        Ok(())
    }

    /// Whether `path` is in the themes folder (so a change there means a reload).
    #[must_use]
    pub fn owns(&self, path: &Path) -> bool {
        path == self.dir || path.parent() == Some(self.dir.as_path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_themes_follow_the_builtins_and_bad_files_are_reported_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.json"), r##"{"name":"Zed","colours":{"accent":"#ff0000"}}"##)
            .unwrap();
        std::fs::write(dir.path().join("a.json"), r#"{"name":"alpha"}"#).unwrap();
        std::fs::write(dir.path().join("bad.json"), "{").unwrap();
        std::fs::write(dir.path().join("clash.json"), r#"{"name":"vscode"}"#).unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not a theme").unwrap();
        let store = ThemeStore::load(dir.path().to_path_buf());
        let names: Vec<&str> = store.all().iter().skip(15).map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["alpha", "Zed"]);
        let files: Vec<&str> = store.unusable.iter().map(|u| u.file.as_str()).collect();
        assert_eq!(files, ["bad.json", "clash.json"]);
        assert_eq!(store.resolve("zed", true).name, "Zed");
        assert_eq!(store.unused_name("VSCode"), "VSCode 2");
    }

    #[test]
    fn a_duplicate_is_saved_edited_and_deleted_and_builtins_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ThemeStore::load(dir.path().join("themes"));
        let vscode = store.get("VSCode").unwrap().clone();
        assert!(store.clone_builtin_refused(&vscode));
        let mut mine = vscode.cloned_as(&store.unused_name("VSCode copy"));
        mine.set("accent", throng_core::project::Colour { r: 1, g: 2, b: 3 });
        store.save(&mine).unwrap();
        let file = dir.path().join("themes/VSCode copy.json");
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("\"accent\": \"#010203\""), "{text}");
        // Someone else's key in the file survives the next save.
        std::fs::write(&file, text.replace("{\n", "{\n  \"author\": \"me\",\n")).unwrap();
        store.reload();
        store.save(&mine).unwrap();
        assert!(std::fs::read_to_string(&file).unwrap().contains("\"author\""));
        assert_eq!(store.get("vscode copy").unwrap().colour("accent"), mine.colour("accent"));
        assert!(store.delete("VSCode").is_err());
        store.delete("VSCode copy").unwrap();
        assert!(store.get("VSCode copy").is_none() && !file.exists());
    }

    impl ThemeStore {
        fn clone_builtin_refused(&mut self, theme: &Theme) -> bool {
            self.save(theme).is_err()
        }
    }
}
