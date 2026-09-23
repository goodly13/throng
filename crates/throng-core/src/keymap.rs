//! Key bindings: the commands a chord can run, where each is live, what each is bound to by
//! default, and the user's `keybindings.json` over those defaults (in the format
//! `{ "version": 1, "bindings": { "navigate.quickOpen": ["Ctrl+Shift+T"] } }`).
//!
//! `Ctrl` in a chord is the platform's command key: Ctrl on Linux and Windows, Cmd on macOS.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::{Map, Value};

/// Where a command is live: the kind of thing that has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scope {
    Editor,
    Terminal,
    Explorer,
    Preview,
    FindInFiles,
}

pub const EVERYWHERE: &[Scope] =
    &[Scope::Editor, Scope::Terminal, Scope::Explorer, Scope::Preview, Scope::FindInFiles];

/// A key with its modifiers.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Chord {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// The key's token: a capital letter, a digit, `F3`, `Enter`, `ArrowUp`, `=`, `` ` ``…
    pub key: String,
}

impl Chord {
    /// Read `Ctrl+Shift+T`. Modifier names are case-blind and have their common aliases (`Cmd`,
    /// `Option`…); a single letter is upper-cased. `None` for a modifier alone or an empty key.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        let token = token.trim();
        // A trailing `+` is the plus key: `Ctrl++`.
        let (mods, key) = match token.strip_suffix("++") {
            Some(head) => (head, "+"),
            None if token == "+" => ("", "+"),
            None => token.rsplit_once('+').unwrap_or(("", token)),
        };
        let mut chord = Chord { ctrl: false, shift: false, alt: false, key: String::new() };
        for part in mods.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "cmd" | "command" | "meta" | "super" => chord.ctrl = true,
                "shift" => chord.shift = true,
                "alt" | "option" | "opt" => chord.alt = true,
                _ => return None,
            }
        }
        let key = key.trim();
        if key.is_empty() || MODIFIER_NAMES.iter().any(|m| m.eq_ignore_ascii_case(key)) {
            return None;
        }
        chord.key = normalise_key(key);
        Some(chord)
    }

    /// Whether a chord can be bound at all: a lone Escape, Space or Enter is the UI's own.
    #[must_use]
    pub fn bindable(&self) -> bool {
        let modified = self.ctrl || self.alt || self.shift;
        modified || !matches!(self.key.as_str(), "Escape" | "Space" | "Enter" | "Tab" | "Backspace")
    }
}

const MODIFIER_NAMES: &[&str] = &["Ctrl", "Control", "Shift", "Alt", "Meta", "Cmd", "Command", "Option"];

fn normalise_key(key: &str) -> String {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return c.to_uppercase().collect();
    }
    let alias = match key.to_ascii_lowercase().as_str() {
        "esc" => "Escape",
        "return" => "Enter",
        "del" => "Delete",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "pgup" => "PageUp",
        "pgdn" | "pgdown" => "PageDown",
        "space" | "spacebar" => "Space",
        _ => "",
    };
    if !alias.is_empty() {
        return alias.to_owned();
    }
    // Named keys keep their case, apart from a lower-case first letter (`f3`, `enter`).
    let mut out = String::with_capacity(key.len());
    let mut chars = key.chars();
    if let Some(first) = chars.next() {
        out.extend(first.to_uppercase());
        out.push_str(chars.as_str());
    }
    out
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("Ctrl+")?;
        }
        if self.shift {
            f.write_str("Shift+")?;
        }
        if self.alt {
            f.write_str("Alt+")?;
        }
        f.write_str(&self.key)
    }
}

/// A command a chord can run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Command {
    pub id: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub scopes: &'static [Scope],
    defaults: &'static [&'static str],
    /// macOS defaults, where they differ (Cmd+H hides the app there).
    mac_defaults: Option<&'static [&'static str]>,
}

impl Command {
    #[must_use]
    pub fn defaults(&self, mac: bool) -> Vec<Chord> {
        let tokens = if mac { self.mac_defaults.unwrap_or(self.defaults) } else { self.defaults };
        tokens.iter().filter_map(|t| Chord::parse(t)).collect()
    }

    #[must_use]
    pub fn live_in(&self, scope: Scope) -> bool {
        self.scopes.contains(&scope)
    }

    fn shares_scope(&self, other: &Command) -> bool {
        self.scopes.iter().any(|s| other.scopes.contains(s))
    }
}

const fn cmd(
    id: &'static str,
    group: &'static str,
    label: &'static str,
    scopes: &'static [Scope],
    defaults: &'static [&'static str],
) -> Command {
    Command { id, group, label, scopes, defaults, mac_defaults: None }
}

const EDITOR: &[Scope] = &[Scope::Editor];
const TERMINAL: &[Scope] = &[Scope::Terminal];
const EXPLORER: &[Scope] = &[Scope::Explorer];
const PANELS: &[Scope] = &[Scope::Editor, Scope::Terminal];
const LINKS: &[Scope] = &[Scope::Editor, Scope::Preview];
const PREVIEW: &[Scope] = &[Scope::Preview];

/// Every command a chord can run, in the order the key bindings editor lists them.
pub const COMMANDS: &[Command] = &[
    cmd("navigate.quickOpen", "Navigate", "Quick Open", EVERYWHERE, &["Ctrl+Shift+T"]),
    cmd("navigate.gotoLine", "Navigate", "Go To Line", EDITOR, &["Ctrl+G"]),
    cmd("preview.followLink", "Navigate", "Open Link", LINKS, &["Ctrl+Enter"]),
    cmd("navigate.back", "Navigate", "Back", PREVIEW, &["Alt+ArrowLeft"]),
    cmd("navigate.forward", "Navigate", "Forward", PREVIEW, &["Alt+ArrowRight"]),
    cmd("tabs.next", "Tabs", "Next tab", EVERYWHERE, &["Ctrl+Tab"]),
    cmd("tabs.previous", "Tabs", "Previous tab", EVERYWHERE, &["Ctrl+Shift+Tab"]),
    cmd("panel.splitRight", "Panels", "Split right with a terminal", EVERYWHERE, &["Ctrl+Shift+D"]),
    cmd("panel.splitDown", "Panels", "Split down with a terminal", EVERYWHERE, &["Ctrl+Shift+E"]),
    cmd("panel.close", "Panels", "Close panel", EVERYWHERE, &["Ctrl+Shift+W"]),
    cmd("project.new", "Projects", "New project", EVERYWHERE, &["Ctrl+Shift+N"]),
    cmd("view.toggleExplorer", "View", "Toggle File Explorer", EVERYWHERE, &["Ctrl+Alt+N"]),
    cmd("view.fullscreen", "View", "Toggle fullscreen", EVERYWHERE, &["F11"]),
    cmd("app.preferences", "View", "Preferences", EVERYWHERE, &["Ctrl+,"]),
    cmd("zoom.in", "Zoom", "Zoom in", EVERYWHERE, &["Ctrl+=", "Ctrl++"]),
    cmd("zoom.out", "Zoom", "Zoom out", EVERYWHERE, &["Ctrl+-"]),
    cmd("zoom.reset", "Zoom", "Reset zoom", EVERYWHERE, &["Ctrl+0"]),
    cmd("editor.save", "Editor", "Save", EDITOR, &["Ctrl+S"]),
    cmd("editor.toggleWordWrap", "Editor", "Toggle word wrap", EDITOR, &["Ctrl+Alt+W"]),
    cmd("search.find", "Search", "Find", PANELS, &["Ctrl+F"]),
    Command {
        mac_defaults: Some(&["Ctrl+Alt+F"]),
        ..cmd("search.replace", "Search", "Find and replace", EDITOR, &["Ctrl+H"])
    },
    cmd("search.findNext", "Search", "Find next", PANELS, &["F3"]),
    cmd("search.findPrevious", "Search", "Find previous", PANELS, &["Shift+F3"]),
    cmd("search.findInFiles", "Search", "Find in Files", EVERYWHERE, &["Ctrl+Shift+F"]),
    cmd("search.replaceInFiles", "Search", "Replace in Files", EVERYWHERE, &["Ctrl+Shift+H"]),
    cmd("terminal.scrollLineUp", "Terminal", "Scroll up a line", TERMINAL, &["Ctrl+Shift+ArrowUp"]),
    cmd("terminal.scrollLineDown", "Terminal", "Scroll down a line", TERMINAL, &["Ctrl+Shift+ArrowDown"]),
    cmd("terminal.scrollPageUp", "Terminal", "Scroll up a page", TERMINAL, &["Shift+PageUp"]),
    cmd("terminal.scrollPageDown", "Terminal", "Scroll down a page", TERMINAL, &["Shift+PageDown"]),
    cmd("terminal.scrollToTop", "Terminal", "Scroll to the top", TERMINAL, &["Ctrl+Home"]),
    cmd("terminal.scrollToBottom", "Terminal", "Scroll to the bottom", TERMINAL, &["Ctrl+End"]),
    cmd("file.rename", "File Explorer", "Rename", EXPLORER, &["F2"]),
    cmd("file.delete", "File Explorer", "Delete", EXPLORER, &["Delete"]),
    cmd("file.cut", "File Explorer", "Cut", EXPLORER, &["Ctrl+X"]),
    cmd("file.copy", "File Explorer", "Copy", EXPLORER, &["Ctrl+C"]),
    cmd("file.paste", "File Explorer", "Paste", EXPLORER, &["Ctrl+V"]),
    cmd("file.undo", "File Explorer", "Undo file operation", EXPLORER, &["Ctrl+Z"]),
    Command {
        // Both chords everywhere; macOS lists its own first, which is what its menus show.
        mac_defaults: Some(&["Ctrl+Shift+Z", "Ctrl+Y"]),
        ..cmd("file.redo", "File Explorer", "Redo file operation", EXPLORER, &["Ctrl+Y", "Ctrl+Shift+Z"])
    },
];

/// Commands throng does not have yet that a `keybindings.json` written for the earlier app may
/// name: passed over silently, since that is no mistake of the user's.
const ELSEWHERE: &[&str] = &[
    "panel.zoomIn",
    "panel.zoomOut",
    "panel.zoomReset",
    "panel.rename",
    "focus.left",
    "focus.right",
    "focus.up",
    "focus.down",
    "focus.cycle",
    "focus.cycleBack",
    "focus.notice",
    "view.toggleProjects",
    "menu.open",
    "tabs.openPicker",
    "preview.open",
    "preview.toggleSyncScroll",
    "navigate.back",
    "navigate.forward",
    "editor.saveAll",
    "editor.saveAs",
    "editor.cutLine",
    "editor.indentLines",
    "editor.outdentLines",
    "editor.columnSelectUp",
    "editor.columnSelectDown",
    "editor.columnSelectLeft",
    "editor.columnSelectRight",
    "search.close",
    "search.replaceCurrent",
    "search.replaceAll",
    "terminal.redraw",
];

#[must_use]
pub fn command(id: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.id == id)
}

/// What taking a chord from a focused terminal costs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalTier {
    Free,
    /// A secondary line-editor binding the user can reach another way: allowed, with a warning.
    Shadowable(&'static str),
    /// The terminal's own and nothing else's: never bound where a terminal has the keyboard.
    Reserved(&'static str),
}

/// The tier of `chord` for a command live in a terminal. On macOS `Ctrl` here is Cmd, which never
/// reaches the program, so nothing is taken.
#[must_use]
pub fn terminal_tier(chord: &Chord, mac: bool) -> TerminalTier {
    if mac || !chord.ctrl || chord.shift || chord.alt {
        return TerminalTier::Free;
    }
    match chord.key.as_str() {
        "C" => TerminalTier::Reserved("interrupt (SIGINT)"),
        "D" => TerminalTier::Reserved("end of input"),
        "Z" => TerminalTier::Reserved("suspend"),
        "A" => TerminalTier::Reserved("start of line"),
        "E" => TerminalTier::Reserved("end of line"),
        "W" => TerminalTier::Reserved("delete word"),
        "U" => TerminalTier::Reserved("delete to line start"),
        "K" => TerminalTier::Reserved("delete to line end"),
        "R" => TerminalTier::Reserved("reverse history search"),
        "L" => TerminalTier::Reserved("clear screen"),
        "Q" => TerminalTier::Reserved("resume output (XON)"),
        "B" => TerminalTier::Shadowable("back a character"),
        "F" => TerminalTier::Shadowable("forward a character"),
        "N" => TerminalTier::Shadowable("next history entry"),
        "P" => TerminalTier::Shadowable("previous history entry"),
        "H" => TerminalTier::Shadowable("backspace"),
        "S" => TerminalTier::Shadowable("pause output (XOFF)"),
        _ => TerminalTier::Free,
    }
}

/// The chords bound to each command: the defaults, with the user's file over them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keymap {
    mac: bool,
    bindings: BTreeMap<&'static str, Vec<Chord>>,
}

impl Keymap {
    #[must_use]
    pub fn defaults(mac: bool) -> Self {
        Self { mac, bindings: COMMANDS.iter().map(|c| (c.id, c.defaults(mac))).collect() }
    }

    /// Read `keybindings.json` over the defaults. Tolerant: an unknown command, a chord that does
    /// not parse, or a wrong-typed value is reported and skipped; the rest applies. A command the
    /// file lists takes exactly the file's chords (an empty list unbinds it).
    #[must_use]
    pub fn parse(text: &str, mac: bool) -> (Self, Vec<String>) {
        let mut map = Self::defaults(mac);
        let mut problems = Vec::new();
        let value: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                problems.push(format!("keybindings.json is not valid JSON ({e}); the defaults apply."));
                return (map, problems);
            }
        };
        let Some(bindings) = value.get("bindings").and_then(Value::as_object) else {
            if !value.is_object() {
                problems.push("keybindings.json is not a JSON object; the defaults apply.".to_owned());
            }
            return (map, problems);
        };
        for (id, tokens) in bindings {
            let Some(command) = command(id) else {
                if !ELSEWHERE.contains(&id.as_str()) {
                    problems.push(format!("\"{id}\" is not a command throng knows."));
                }
                continue;
            };
            let Some(tokens) = tokens.as_array() else {
                problems.push(format!("\"{id}\" should be a list of chords."));
                continue;
            };
            let mut chords = Vec::new();
            for token in tokens {
                match token.as_str().and_then(Chord::parse) {
                    Some(chord) if chords.contains(&chord) => {}
                    Some(chord) => match map.refusal(id, &chord) {
                        Some(reason) => problems.push(format!("\"{id}\": {reason}")),
                        None => chords.push(chord),
                    },
                    None => problems.push(format!("\"{id}\": {token} is not a chord.")),
                }
            }
            map.bindings.insert(command.id, chords);
        }
        (map, problems)
    }

    /// This keymap written over an existing file's JSON: commands at their defaults are left out
    /// of `bindings` (so a later change to a default reaches them); everything else the file holds,
    /// commands throng does not know included, is kept.
    #[must_use]
    pub fn write_into(&self, existing: Option<&str>) -> String {
        let mut doc = existing
            .and_then(|t| serde_json::from_str::<Value>(t).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        doc.entry("version").or_insert(Value::from(1));
        let mut bindings = doc.get("bindings").and_then(Value::as_object).cloned().unwrap_or_else(Map::new);
        for command in COMMANDS {
            let chords = self.chords(command.id);
            if chords == command.defaults(self.mac).as_slice() {
                bindings.remove(command.id);
            } else {
                let tokens = chords.iter().map(|c| Value::String(c.to_string())).collect();
                bindings.insert(command.id.to_owned(), Value::Array(tokens));
            }
        }
        doc.insert("bindings".into(), Value::Object(bindings));
        let mut text = serde_json::to_string_pretty(&Value::Object(doc)).expect("a JSON object serialises");
        text.push('\n');
        text
    }

    #[must_use]
    pub fn chords(&self, id: &str) -> &[Chord] {
        self.bindings.get(id).map_or(&[], Vec::as_slice)
    }

    #[must_use]
    pub fn is_default(&self, id: &str) -> bool {
        command(id).is_some_and(|c| self.chords(id) == c.defaults(self.mac).as_slice())
    }

    /// The command `chord` runs where `scope` has the keyboard.
    #[must_use]
    pub fn lookup(&self, chord: &Chord, scope: Scope) -> Option<&'static str> {
        COMMANDS.iter().find(|c| c.live_in(scope) && self.chords(c.id).contains(chord)).map(|c| c.id)
    }

    /// Commands other than `id` that `chord` already runs somewhere `id` is live too.
    #[must_use]
    pub fn conflicts(&self, id: &str, chord: &Chord) -> Vec<&'static str> {
        let Some(mine) = command(id) else { return Vec::new() };
        COMMANDS
            .iter()
            .filter(|c| c.id != id && c.shares_scope(mine) && self.chords(c.id).contains(chord))
            .map(|c| c.id)
            .collect()
    }

    /// Why `chord` may not be bound to `id`, if it may not: it cannot be bound at all, or the
    /// command is live in a terminal and the chord is the terminal's.
    #[must_use]
    pub fn refusal(&self, id: &str, chord: &Chord) -> Option<String> {
        if !chord.bindable() {
            return Some(format!("{chord} on its own belongs to the interface."));
        }
        let command = command(id)?;
        match terminal_tier(chord, self.mac) {
            TerminalTier::Reserved(what) if command.live_in(Scope::Terminal) => Some(format!(
                "{chord} is the terminal's ({what}) and {} is live in terminals.",
                command.label
            )),
            _ => None,
        }
    }

    /// Add `chord` to `id`, taking it from any command it conflicts with.
    pub fn bind(&mut self, id: &str, chord: Chord) {
        let Some(command) = command(id) else { return };
        for other in self.conflicts(id, &chord) {
            if let Some(chords) = self.bindings.get_mut(other) {
                chords.retain(|c| c != &chord);
            }
        }
        let chords = self.bindings.entry(command.id).or_default();
        if !chords.contains(&chord) {
            chords.push(chord);
        }
    }

    pub fn unbind(&mut self, id: &str, chord: &Chord) {
        if let Some(chords) = self.bindings.get_mut(id) {
            chords.retain(|c| c != chord);
        }
    }

    pub fn reset(&mut self, id: &str) {
        if let Some(command) = command(id) {
            self.bindings.insert(command.id, command.defaults(self.mac));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(t: &str) -> Chord {
        Chord::parse(t).unwrap()
    }

    #[test]
    fn chords_parse_with_aliases_and_print_in_one_order() {
        assert_eq!(chord("shift+ctrl+t").to_string(), "Ctrl+Shift+T");
        assert_eq!(chord("Cmd+Option+f").to_string(), "Ctrl+Alt+F");
        assert_eq!(chord("Ctrl++").key, "+");
        assert_eq!(chord("Ctrl+=").key, "=");
        assert_eq!(chord("esc").key, "Escape");
        assert_eq!(chord("f3").key, "F3");
        assert_eq!(chord("Alt+Up").key, "ArrowUp");
        assert!(Chord::parse("Ctrl+").is_none() && Chord::parse("Ctrl+Shift").is_none());
        assert!(Chord::parse("Hyper+X").is_none());
        assert!(!chord("Escape").bindable() && chord("Ctrl+Escape").bindable() && chord("F2").bindable());
    }

    #[test]
    fn every_default_parses_and_no_two_commands_share_a_chord_where_both_are_live() {
        for mac in [false, true] {
            let map = Keymap::defaults(mac);
            for c in COMMANDS {
                let tokens = if mac { c.mac_defaults.unwrap_or(c.defaults) } else { c.defaults };
                assert_eq!(c.defaults(mac).len(), tokens.len(), "{} has a default that does not parse", c.id);
                for chord in map.chords(c.id) {
                    assert_eq!(map.conflicts(c.id, chord), Vec::<&str>::new(), "{} {chord}", c.id);
                    assert_eq!(map.refusal(c.id, chord), None, "{} {chord}", c.id);
                }
            }
        }
    }

    #[test]
    fn the_file_overrides_defaults_and_its_problems_are_reported_not_fatal() {
        let text = r#"{"version":1,"bindings":{
            "navigate.quickOpen":["Ctrl+P"],
            "search.findInFiles":[],
            "no.such":["Ctrl+Q"],
            "zoom.in":"Ctrl+=",
            "panel.close":["Ctrl+Shift+W","Nonsense+"],
            "tabs.next":["Ctrl+R","Ctrl+PageDown"],
            "focus.left":["Ctrl+Alt+ArrowLeft"]}}"#;
        let (map, problems) = Keymap::parse(text, false);
        assert_eq!(map.chords("navigate.quickOpen"), [chord("Ctrl+P")]);
        assert!(map.chords("search.findInFiles").is_empty(), "an empty list unbinds");
        assert_eq!(map.chords("zoom.in"), Keymap::defaults(false).chords("zoom.in"));
        assert_eq!(map.chords("panel.close"), [chord("Ctrl+Shift+W")]);
        assert_eq!(map.chords("tabs.next"), [chord("Ctrl+PageDown")], "the terminal keeps Ctrl+R");
        assert_eq!(problems.len(), 4, "{problems:?}");
        let (map, problems) = Keymap::parse("{", false);
        assert_eq!(map, Keymap::defaults(false));
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn lookup_honours_scope_and_mac_has_its_own_defaults() {
        let map = Keymap::defaults(false);
        assert_eq!(map.lookup(&chord("Ctrl+Z"), Scope::Explorer), Some("file.undo"));
        assert_eq!(map.lookup(&chord("Ctrl+Z"), Scope::Terminal), None, "the shell's suspend");
        assert_eq!(map.lookup(&chord("Ctrl+G"), Scope::Terminal), None);
        assert_eq!(map.lookup(&chord("Ctrl+G"), Scope::Editor), Some("navigate.gotoLine"));
        let mac = Keymap::defaults(true);
        assert_eq!(mac.lookup(&chord("Ctrl+H"), Scope::Editor), None, "Cmd+H hides the app");
        for redo in ["Ctrl+Y", "Ctrl+Shift+Z"] {
            assert_eq!(mac.lookup(&chord(redo), Scope::Explorer), Some("file.redo"), "{redo} on macOS");
            assert_eq!(map.lookup(&chord(redo), Scope::Explorer), Some("file.redo"), "{redo}");
        }
        assert_eq!(mac.lookup(&chord("Ctrl+Alt+F"), Scope::Editor), Some("search.replace"));
    }

    #[test]
    fn binding_takes_a_chord_from_its_rival_and_refuses_the_terminals_own() {
        let mut map = Keymap::defaults(false);
        assert_eq!(map.conflicts("navigate.quickOpen", &chord("Ctrl+Shift+F")), ["search.findInFiles"]);
        map.bind("navigate.quickOpen", chord("Ctrl+Shift+F"));
        assert!(map.chords("search.findInFiles").is_empty());
        assert_eq!(map.lookup(&chord("Ctrl+Shift+F"), Scope::Terminal), Some("navigate.quickOpen"));
        // Explorer-only Ctrl+Z and an editor-only chord never meet, so they do not conflict.
        assert!(map.conflicts("navigate.gotoLine", &chord("Ctrl+Z")).is_empty());
        assert!(map.refusal("navigate.quickOpen", &chord("Ctrl+R")).is_some());
        assert!(map.refusal("navigate.gotoLine", &chord("Ctrl+R")).is_none(), "an editor-only command may");
        assert!(Keymap::defaults(true).refusal("navigate.quickOpen", &chord("Ctrl+R")).is_none(), "Cmd+R");
        assert_eq!(terminal_tier(&chord("Ctrl+B"), false), TerminalTier::Shadowable("back a character"));
        map.reset("search.findInFiles");
        assert_eq!(map.chords("search.findInFiles"), [chord("Ctrl+Shift+F")]);
    }

    #[test]
    fn writing_keeps_only_changes_and_whatever_else_the_file_holds() {
        let existing = r#"{"version":1,"comment":"mine","bindings":{"someone.elses":["Ctrl+Q"]}}"#;
        let (mut map, _) = Keymap::parse(existing, false);
        map.unbind("view.fullscreen", &chord("F11"));
        map.bind("view.fullscreen", chord("Ctrl+Alt+Enter"));
        let written: Value = serde_json::from_str(&map.write_into(Some(existing))).unwrap();
        assert_eq!(written["comment"], "mine");
        assert_eq!(written["bindings"]["someone.elses"], serde_json::json!(["Ctrl+Q"]));
        assert_eq!(written["bindings"]["view.fullscreen"], serde_json::json!(["Ctrl+Alt+Enter"]));
        assert!(written["bindings"].get("navigate.quickOpen").is_none(), "defaults stay out");
        let (again, problems) = Keymap::parse(&map.write_into(Some(existing)), false);
        assert_eq!(again, map);
        assert_eq!(problems.len(), 1, "the unknown command is still reported, and still kept");
    }
}
