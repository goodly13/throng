//! Application settings, declared once as metadata (Principle X).
//!
//! The read policy, applied to every document:
//!
//! - **absent** → shipped defaults, and the caller writes them out;
//! - **malformed** (not JSON, or not an object) → defaults *in memory only*; the file is left
//!   byte-for-byte alone, because writing defaults over a file the user is halfway through fixing is
//!   worse than running on defaults for a moment;
//! - **parseable but out of bounds** → each bad value is clamped or replaced, and the corrected
//!   document is written back once.
//!
//! Keys this build does not model survive every write (a hand-added key is legitimate); keys in
//! [`RETIRED_KEYS`] are dropped on the next write.

use serde_json::{Map, Value};

/// A setting's type, bounds and default.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SettingKind {
    Bool {
        default: bool,
    },
    Int {
        min: i64,
        max: i64,
        default: i64,
    },
    Float {
        min: f64,
        max: f64,
        default: f64,
        step: f64,
    },
    Choice {
        options: &'static [&'static str],
        default: &'static str,
    },
    /// An optional free-text value (e.g. a shell id); `null` or absent means unset.
    OptText,
    /// Free text naming something only known at run time (a theme, an icon pack). Whether it names
    /// anything is the reader's call: an unknown name falls back where it is used, and the file
    /// keeps what the user wrote.
    Text {
        default: &'static str,
    },
    List {
        default: &'static [&'static str],
    },
}

/// One setting's metadata.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SettingDef {
    /// Dotted path, e.g. `terminal.fontSize`.
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: SettingKind,
}

/// Every setting throng models, in the order the preferences editor shows them.
pub const SETTINGS: &[SettingDef] = &[
    SettingDef {
        key: "appearance.theme",
        label: "Theme",
        help: "The theme the whole app is drawn in. \"system\" follows the operating system's light or dark mode.",
        kind: SettingKind::Text { default: "throng" },
    },
    SettingDef {
        key: "appearance.iconPack",
        label: "Icon pack",
        help: "Glyphs for the file tree and toolbar icons, from a pack in the icon-packs folder. Empty uses throng's own.",
        kind: SettingKind::Text { default: "" },
    },
    SettingDef {
        key: "appearance.uiScale",
        label: "Interface scale",
        help: "Scales the whole interface.",
        kind: SettingKind::Float { min: 0.75, max: 2.0, default: 1.0, step: 0.05 },
    },
    SettingDef {
        key: "terminal.fontSize",
        label: "Terminal font size",
        help: "Point size of terminal text.",
        kind: SettingKind::Float { min: 8.0, max: 36.0, default: 14.0, step: 0.5 },
    },
    SettingDef {
        key: "terminal.scrollbackLines",
        label: "Scrollback lines",
        help: "Lines kept above the screen in each terminal.",
        kind: SettingKind::Int { min: 1_000, max: 200_000, default: 10_000 },
    },
    SettingDef {
        key: "terminal.defaultShell",
        label: "Default shell",
        help: "The shell a new terminal starts with. Empty uses your login shell.",
        kind: SettingKind::OptText,
    },
    SettingDef {
        key: "terminal.defaultRememberDirectory",
        label: "Reopen terminals in their last directory",
        help: "A terminal starts again in the folder it was last working in, while that folder is in the project.",
        kind: SettingKind::Bool { default: true },
    },
    SettingDef {
        key: "terminal.reloadMode",
        label: "Start terminals",
        help: "Automatic starts a project's terminals when it opens. Manual leaves them stopped until \
               you reload each one. A terminal still running reattaches either way.",
        kind: SettingKind::Choice { options: &["automatic", "manual"], default: "automatic" },
    },
    SettingDef {
        key: "terminal.showStatusBar",
        label: "Show terminal status bar",
        help: "A strip under each terminal with its shell and working directory.",
        kind: SettingKind::Bool { default: true },
    },
    SettingDef {
        key: "terminal.copyOnSelect",
        label: "Copy on select",
        help: "Copy terminal text to the clipboard as soon as it is selected.",
        kind: SettingKind::Bool { default: false },
    },
    SettingDef {
        key: "editor.fontSize",
        label: "Editor font size",
        help: "Point size of editor text.",
        kind: SettingKind::Float { min: 8.0, max: 36.0, default: 14.0, step: 0.5 },
    },
    SettingDef {
        key: "editor.tabSize",
        label: "Tab size",
        help: "Columns per indentation level for new files.",
        kind: SettingKind::Int { min: 1, max: 16, default: 4 },
    },
    SettingDef {
        key: "editor.wordWrap",
        label: "Editor default word wrap",
        help: "Wrap long lines at the panel edge in a file opened fresh. Ctrl+Alt+W toggles one document.",
        kind: SettingKind::Bool { default: true },
    },
    SettingDef {
        key: "editor.defaultLineEnding",
        label: "Line ending for new files",
        help: "Existing files always keep their own.",
        kind: SettingKind::Choice { options: &["lf", "crlf"], default: "lf" },
    },
    SettingDef {
        key: "editor.persistUndoHistory",
        label: "Keep undo history through a restart",
        help: "Unsaved edits always survive a crash or quit; this also keeps their undo history.",
        kind: SettingKind::Bool { default: true },
    },
    SettingDef {
        key: "editor.autoSave",
        label: "Save automatically",
        help: "Save a changed file shortly after typing stops. Untitled files wait for Save As.",
        kind: SettingKind::Bool { default: false },
    },
    SettingDef {
        key: "editor.autoSaveDebounceMs",
        label: "Auto-save waits for typing to stop (ms)",
        help: "How long after the last edit an automatic save happens.",
        kind: SettingKind::Int { min: 0, max: 10_000, default: 300 },
    },
    SettingDef {
        key: "editor.links.detectInEditors",
        label: "Show links in editors",
        help: "Underline file paths, web addresses and mail links in documents; Ctrl+click follows one.",
        kind: SettingKind::Bool { default: true },
    },
    SettingDef {
        key: "editor.links.detectInTerminals",
        label: "Show links in terminals",
        help: "Underline file paths, web addresses and hyperlinks in terminal output; Ctrl+click follows one.",
        kind: SettingKind::Bool { default: true },
    },
    SettingDef {
        key: "editor.previews.updateDelayMs",
        label: "Preview update delay (ms)",
        help: "How long after the last edit a preview beside its editor updates.",
        kind: SettingKind::Int { min: 0, max: 5_000, default: 300 },
    },
    SettingDef {
        key: "editor.previews.maxWaitMs",
        label: "Preview maximum wait (ms)",
        help: "The longest a preview goes without showing an edit while typing continues.",
        kind: SettingKind::Int { min: 0, max: 10_000, default: 1_000 },
    },
    SettingDef {
        key: "editor.showStatusBar",
        label: "Show editor status bar",
        help: "A strip under each editor with the caret, counts, language, wrap and preview.",
        kind: SettingKind::Bool { default: true },
    },
    SettingDef {
        key: "editor.statusBar.showCursorPosition",
        label: "Status bar shows the caret",
        help: "Line, column and selected characters.",
        kind: SettingKind::Bool { default: true },
    },
    SettingDef {
        key: "editor.statusBar.showCounts",
        label: "Status bar shows counts",
        help: "The document's characters and words.",
        kind: SettingKind::Bool { default: true },
    },
    SettingDef {
        key: "editor.maxOpenFileBytes",
        label: "Largest file to open",
        help: "Files larger than this open with a notice instead of an editor.",
        kind: SettingKind::Int { min: 5_242_880, max: 1_073_741_824, default: 10_485_760 },
    },
    SettingDef {
        key: "explorer.exclude",
        label: "Hidden from the file tree",
        help: "File and folder names hidden in every project.",
        kind: SettingKind::List {
            default: &[".git", ".svn", ".hg", "CVS", ".DS_Store", "Thumbs.db", "node_modules"],
        },
    },
    SettingDef {
        key: "search.settleMs",
        label: "Find in Files waits for typing to settle (ms)",
        help: "How long after the last keystroke a Find in Files search starts.",
        kind: SettingKind::Int { min: 100, max: 5_000, default: 500 },
    },
    SettingDef {
        key: "search.warnIrreversibleCommit",
        label: "Warn before replacing in files that are not open",
        help: "Replacements in unopened files are written straight to disk and cannot be undone in throng.",
        kind: SettingKind::Bool { default: true },
    },
    SettingDef {
        key: "search.quickOpenExcludeHidden",
        label: "Quick Open leaves out files hidden in the project",
        help: "Quick Open's starting state for files under \"Hide in this project\".",
        kind: SettingKind::Bool { default: true },
    },
];

/// Keys that used to exist and are deliberately dropped on the next write.
pub const RETIRED_KEYS: &[&str] = &[];

/// A setting's value.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    OptText(Option<String>),
    List(Vec<String>),
}

impl SettingValue {
    fn to_json(&self) -> Value {
        match self {
            Self::Bool(b) => Value::Bool(*b),
            Self::Int(i) => Value::from(*i),
            Self::Float(f) => serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number),
            Self::Text(s) => Value::String(s.clone()),
            Self::OptText(s) => s.clone().map_or(Value::Null, Value::String),
            Self::List(items) => Value::Array(items.iter().cloned().map(Value::String).collect()),
        }
    }
}

/// How a document was read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadOutcome {
    /// No file: running on defaults, and they should be written out.
    Missing,
    /// Unreadable: running on defaults; the file must not be touched.
    Malformed { reason: String },
    /// Read. `corrected` names the keys that were clamped or replaced and should be written back.
    Loaded { corrected: Vec<&'static str> },
}

impl ReadOutcome {
    /// Whether the caller should write the document after this read.
    #[must_use]
    pub fn needs_write(&self) -> bool {
        match self {
            Self::Missing => true,
            Self::Malformed { .. } => false,
            Self::Loaded { corrected } => !corrected.is_empty(),
        }
    }
}

/// The settings document: the modelled values plus the raw object they came from, so unmodelled
/// keys survive a write.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    doc: Map<String, Value>,
    values: Vec<(&'static str, SettingValue)>,
}

impl Default for Settings {
    fn default() -> Self {
        let mut settings = Self { doc: Map::new(), values: Vec::new() };
        for def in SETTINGS {
            let value = default_value(def.kind);
            set_path(&mut settings.doc, def.key, value.to_json());
            settings.values.push((def.key, value));
        }
        settings
    }
}

fn default_value(kind: SettingKind) -> SettingValue {
    match kind {
        SettingKind::Bool { default } => SettingValue::Bool(default),
        SettingKind::Int { default, .. } => SettingValue::Int(default),
        SettingKind::Float { default, .. } => SettingValue::Float(default),
        SettingKind::Choice { default, .. } => SettingValue::Text(default.to_owned()),
        SettingKind::OptText => SettingValue::OptText(None),
        SettingKind::Text { default } => SettingValue::Text(default.to_owned()),
        SettingKind::List { default } => {
            SettingValue::List(default.iter().map(|s| (*s).to_owned()).collect())
        }
    }
}

/// Read one value against its definition: `(value, corrected)`.
fn coerce(kind: SettingKind, raw: Option<&Value>) -> (SettingValue, bool) {
    let Some(raw) = raw else { return (default_value(kind), false) };
    match kind {
        SettingKind::Bool { default } => match raw.as_bool() {
            Some(b) => (SettingValue::Bool(b), false),
            None => (SettingValue::Bool(default), true),
        },
        SettingKind::Int { min, max, default } => match raw.as_f64() {
            Some(f) if f.is_finite() => {
                let rounded = f.round() as i64;
                let clamped = rounded.clamp(min, max);
                (SettingValue::Int(clamped), clamped as f64 != f)
            }
            _ => (SettingValue::Int(default), true),
        },
        SettingKind::Float { min, max, default, .. } => match raw.as_f64() {
            Some(f) if f.is_finite() => {
                let clamped = f.clamp(min, max);
                (SettingValue::Float(clamped), clamped != f)
            }
            _ => (SettingValue::Float(default), true),
        },
        SettingKind::Choice { options, default } => match raw.as_str() {
            Some(s) if options.contains(&s) => (SettingValue::Text(s.to_owned()), false),
            _ => (SettingValue::Text(default.to_owned()), true),
        },
        SettingKind::OptText => match raw {
            Value::Null => (SettingValue::OptText(None), false),
            Value::String(s) if s.trim().is_empty() => (SettingValue::OptText(None), false),
            Value::String(s) => (SettingValue::OptText(Some(s.clone())), false),
            _ => (SettingValue::OptText(None), true),
        },
        SettingKind::Text { default } => match raw.as_str() {
            Some(s) => (SettingValue::Text(s.to_owned()), false),
            None => (SettingValue::Text(default.to_owned()), true),
        },
        SettingKind::List { default } => match raw.as_array() {
            Some(items) if items.iter().all(Value::is_string) => (
                SettingValue::List(items.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()),
                false,
            ),
            _ => (SettingValue::List(default.iter().map(|s| (*s).to_owned()).collect()), true),
        },
    }
}

fn get_path<'a>(doc: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    let mut parts = key.split('.');
    let mut current = doc.get(parts.next()?)?;
    for part in parts {
        current = current.as_object()?.get(part)?;
    }
    Some(current)
}

fn set_path(doc: &mut Map<String, Value>, key: &str, value: Value) {
    let parts: Vec<&str> = key.split('.').collect();
    let mut current = doc;
    for part in &parts[..parts.len() - 1] {
        let entry = current.entry((*part).to_owned()).or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        current = entry.as_object_mut().expect("just ensured an object");
    }
    current.insert(parts[parts.len() - 1].to_owned(), value);
}

fn remove_path(doc: &mut Map<String, Value>, key: &str) {
    let parts: Vec<&str> = key.split('.').collect();
    let mut current = doc;
    for part in &parts[..parts.len() - 1] {
        match current.get_mut(*part).and_then(Value::as_object_mut) {
            Some(next) => current = next,
            None => return,
        }
    }
    current.remove(parts[parts.len() - 1]);
}

impl Settings {
    /// Read a settings document. `None` means the file does not exist.
    #[must_use]
    pub fn read(text: Option<&str>) -> (Self, ReadOutcome) {
        let Some(text) = text else { return (Self::default(), ReadOutcome::Missing) };
        let parsed: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => return (Self::default(), ReadOutcome::Malformed { reason: e.to_string() }),
        };
        let Value::Object(mut doc) = parsed else {
            // Not correctable: a document that is not even an object is left for the user to fix,
            // never reported as "corrected" (which would trigger a write of defaults over it).
            return (
                Self::default(),
                ReadOutcome::Malformed { reason: "settings must be a JSON object".into() },
            );
        };
        let mut values = Vec::new();
        let mut corrected = Vec::new();
        for def in SETTINGS {
            let (value, fixed) = coerce(def.kind, get_path(&doc, def.key));
            if fixed {
                corrected.push(def.key);
                set_path(&mut doc, def.key, value.to_json());
            }
            values.push((def.key, value));
        }
        (Self { doc, values }, ReadOutcome::Loaded { corrected })
    }

    /// The document to write: unmodelled keys kept, retired keys dropped, modelled keys current.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut doc = self.doc.clone();
        for key in RETIRED_KEYS {
            remove_path(&mut doc, key);
        }
        for (key, value) in &self.values {
            set_path(&mut doc, key, value.to_json());
        }
        let mut text = serde_json::to_string_pretty(&Value::Object(doc)).expect("a JSON object serialises");
        text.push('\n');
        text
    }

    /// Set a modelled key, clamping it to its bounds. Returns false for an unknown key or a value of
    /// the wrong type.
    pub fn set(&mut self, key: &str, value: SettingValue) -> bool {
        let Some(def) = SETTINGS.iter().find(|d| d.key == key) else { return false };
        let (coerced, rejected) = coerce(def.kind, Some(&value.to_json()));
        if rejected && std::mem::discriminant(&coerced) != std::mem::discriminant(&value) {
            return false;
        }
        if let Some(slot) = self.values.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = coerced.clone();
        }
        set_path(&mut self.doc, key, coerced.to_json());
        true
    }

    /// Reset one key to its default.
    pub fn reset(&mut self, key: &str) {
        if let Some(def) = SETTINGS.iter().find(|d| d.key == key) {
            self.set(key, default_value(def.kind));
        }
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&SettingValue> {
        self.values.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    fn bool(&self, key: &str) -> bool {
        matches!(self.get(key), Some(SettingValue::Bool(true)))
    }

    fn float(&self, key: &str) -> f32 {
        match self.get(key) {
            Some(SettingValue::Float(f)) => *f as f32,
            Some(SettingValue::Int(i)) => *i as f32,
            _ => 0.0,
        }
    }

    fn int(&self, key: &str) -> i64 {
        match self.get(key) {
            Some(SettingValue::Int(i)) => *i,
            _ => 0,
        }
    }

    fn text(&self, key: &str) -> &str {
        match self.get(key) {
            Some(SettingValue::Text(s)) => s,
            _ => "",
        }
    }

    #[must_use]
    pub fn theme(&self) -> &str {
        self.text("appearance.theme")
    }
    #[must_use]
    pub fn icon_pack(&self) -> &str {
        self.text("appearance.iconPack")
    }
    #[must_use]
    pub fn ui_scale(&self) -> f32 {
        self.float("appearance.uiScale")
    }
    #[must_use]
    pub fn terminal_font_size(&self) -> f32 {
        self.float("terminal.fontSize")
    }
    #[must_use]
    pub fn scrollback_lines(&self) -> usize {
        usize::try_from(self.int("terminal.scrollbackLines")).unwrap_or(10_000)
    }
    #[must_use]
    pub fn default_shell(&self) -> Option<&str> {
        match self.get("terminal.defaultShell") {
            Some(SettingValue::OptText(Some(s))) => Some(s),
            _ => None,
        }
    }
    #[must_use]
    pub fn copy_on_select(&self) -> bool {
        self.bool("terminal.copyOnSelect")
    }
    #[must_use]
    pub fn editor_font_size(&self) -> f32 {
        self.float("editor.fontSize")
    }
    #[must_use]
    pub fn tab_size(&self) -> usize {
        usize::try_from(self.int("editor.tabSize")).unwrap_or(4)
    }
    #[must_use]
    pub fn word_wrap(&self) -> bool {
        self.bool("editor.wordWrap")
    }
    #[must_use]
    pub fn default_line_ending(&self) -> crate::text::LineEnding {
        if self.text("editor.defaultLineEnding") == "crlf" {
            crate::text::LineEnding::CrLf
        } else {
            crate::text::LineEnding::Lf
        }
    }
    #[must_use]
    pub fn max_open_file_bytes(&self) -> u64 {
        u64::try_from(self.int("editor.maxOpenFileBytes")).unwrap_or(10_485_760)
    }
    #[must_use]
    pub fn persist_undo_history(&self) -> bool {
        self.bool("editor.persistUndoHistory")
    }
    #[must_use]
    pub fn links_in_editors(&self) -> bool {
        self.bool("editor.links.detectInEditors")
    }
    #[must_use]
    pub fn links_in_terminals(&self) -> bool {
        self.bool("editor.links.detectInTerminals")
    }
    /// The preview update delay and maximum wait; the wait is never shorter than the delay.
    #[must_use]
    pub fn preview_timing(&self) -> (u64, u64) {
        let delay = u64::try_from(self.int("editor.previews.updateDelayMs")).unwrap_or(300);
        let wait = u64::try_from(self.int("editor.previews.maxWaitMs")).unwrap_or(1_000);
        (delay, wait.max(delay))
    }
    #[must_use]
    pub fn remember_directory(&self) -> bool {
        self.bool("terminal.defaultRememberDirectory")
    }
    /// Whether a project's terminals wait to be reloaded rather than starting when it opens.
    #[must_use]
    pub fn manual_reload(&self) -> bool {
        self.text("terminal.reloadMode") == "manual"
    }
    /// Whether editors show their status strip, and which readouts it carries: (strip, caret,
    /// counts).
    #[must_use]
    pub fn editor_status_bar(&self) -> (bool, bool, bool) {
        (
            self.bool("editor.showStatusBar"),
            self.bool("editor.statusBar.showCursorPosition"),
            self.bool("editor.statusBar.showCounts"),
        )
    }
    #[must_use]
    pub fn terminal_status_bar(&self) -> bool {
        self.bool("terminal.showStatusBar")
    }
    #[must_use]
    pub fn auto_save(&self) -> bool {
        self.bool("editor.autoSave")
    }
    #[must_use]
    pub fn auto_save_debounce_ms(&self) -> u64 {
        u64::try_from(self.int("editor.autoSaveDebounceMs")).unwrap_or(300)
    }
    #[must_use]
    pub fn search_settle_ms(&self) -> u64 {
        u64::try_from(self.int("search.settleMs")).unwrap_or(500)
    }
    #[must_use]
    pub fn warn_irreversible_commit(&self) -> bool {
        self.bool("search.warnIrreversibleCommit")
    }
    #[must_use]
    pub fn quick_open_exclude_hidden(&self) -> bool {
        self.bool("search.quickOpenExcludeHidden")
    }
    #[must_use]
    pub fn explorer_exclude(&self) -> Vec<String> {
        match self.get("explorer.exclude") {
            Some(SettingValue::List(items)) => items.clone(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_uses_defaults_and_asks_for_a_write() {
        let (s, outcome) = Settings::read(None);
        assert_eq!(outcome, ReadOutcome::Missing);
        assert!(outcome.needs_write());
        assert!((s.terminal_font_size() - 14.0).abs() < f32::EPSILON);
        assert_eq!(s.theme(), "throng");
    }

    #[test]
    fn a_theme_name_is_kept_as_written_and_only_a_non_string_is_corrected() {
        let (s, outcome) = Settings::read(Some(r#"{"appearance":{"theme":"My Own"}}"#));
        assert_eq!(outcome, ReadOutcome::Loaded { corrected: vec![] });
        assert_eq!(s.theme(), "My Own", "whether it names a theme is decided where themes are known");
        let (s, outcome) = Settings::read(Some(r#"{"appearance":{"theme":3}}"#));
        assert_eq!(outcome, ReadOutcome::Loaded { corrected: vec!["appearance.theme"] });
        assert_eq!(s.theme(), "throng");
    }

    #[test]
    fn malformed_file_is_never_rewritten() {
        for text in ["{", "[]", "\"x\"", "null", ""] {
            let (s, outcome) = Settings::read(Some(text));
            assert!(matches!(outcome, ReadOutcome::Malformed { .. }), "{text:?}");
            assert!(!outcome.needs_write(), "{text:?}");
            assert_eq!(s, Settings::default());
        }
    }

    #[test]
    fn out_of_bounds_values_are_clamped_and_reported() {
        let text = r#"{"terminal":{"fontSize":500,"scrollbackLines":"lots"},"editor":{"tabSize":0}}"#;
        let (s, outcome) = Settings::read(Some(text));
        let ReadOutcome::Loaded { corrected } = &outcome else { panic!("{outcome:?}") };
        assert_eq!(corrected, &vec!["terminal.fontSize", "terminal.scrollbackLines", "editor.tabSize"]);
        assert!((s.terminal_font_size() - 36.0).abs() < f32::EPSILON);
        assert_eq!(s.scrollback_lines(), 10_000);
        assert_eq!(s.tab_size(), 1);
        assert!(outcome.needs_write());
    }

    #[test]
    fn valid_file_needs_no_write_and_absent_keys_default() {
        let (s, outcome) = Settings::read(Some(r#"{"editor":{"wordWrap":true}}"#));
        assert_eq!(outcome, ReadOutcome::Loaded { corrected: vec![] });
        assert!(!outcome.needs_write());
        assert!(s.word_wrap());
        assert_eq!(s.tab_size(), 4);
    }

    #[test]
    fn unmodelled_keys_survive_a_write() {
        let text = r#"{"myOwnKey":{"nested":[1,2]},"terminal":{"fontSize":12,"customFlag":true}}"#;
        let (mut s, _) = Settings::read(Some(text));
        s.set("terminal.fontSize", SettingValue::Float(16.0));
        let written: Value = serde_json::from_str(&s.to_json()).unwrap();
        assert_eq!(written["myOwnKey"]["nested"], serde_json::json!([1, 2]));
        assert_eq!(written["terminal"]["customFlag"], Value::Bool(true));
        assert_eq!(written["terminal"]["fontSize"], serde_json::json!(16.0));
    }

    #[test]
    fn set_clamps_and_rejects_wrong_types() {
        let mut s = Settings::default();
        assert!(s.set("editor.tabSize", SettingValue::Int(99)));
        assert_eq!(s.tab_size(), 16);
        assert!(!s.set("editor.tabSize", SettingValue::Text("x".into())));
        assert!(!s.set("no.such.key", SettingValue::Bool(true)));
        assert!(s.set("terminal.defaultShell", SettingValue::OptText(Some("zsh".into()))));
        assert_eq!(s.default_shell(), Some("zsh"));
        s.reset("terminal.defaultShell");
        assert_eq!(s.default_shell(), None);
    }

    #[test]
    fn every_default_is_within_its_own_bounds() {
        for def in SETTINGS {
            let (value, corrected) = coerce(def.kind, Some(&default_value(def.kind).to_json()));
            assert!(!corrected, "{} default is out of bounds", def.key);
            assert_eq!(value, default_value(def.kind), "{}", def.key);
        }
    }
}
