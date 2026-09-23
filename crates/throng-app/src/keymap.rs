//! The key bindings in the running app: key presses become chords, chords run commands. The
//! keymap lives in the egui context, so every place that owns a command asks it the same way.

use std::sync::Arc;

use egui::{Context, Event, Id, Key, Modifiers};
use throng_core::keymap::{Chord, Keymap, Scope, command};

fn id() -> Id {
    Id::new("throng-keymap")
}

#[must_use]
pub fn mac() -> bool {
    cfg!(target_os = "macos")
}

/// Make `map` the keymap every command is looked up in.
pub fn install(ctx: &Context, map: Arc<Keymap>) {
    ctx.data_mut(|d| d.insert_temp(id(), map));
}

/// The keymap in force (the defaults until one is installed).
#[must_use]
pub fn get(ctx: &Context) -> Arc<Keymap> {
    ctx.data(|d| d.get_temp::<Arc<Keymap>>(id())).unwrap_or_else(|| Arc::new(Keymap::defaults(mac())))
}

/// A key's token in a chord.
#[must_use]
pub fn token(key: Key) -> String {
    match key {
        Key::ArrowUp => "ArrowUp",
        Key::ArrowDown => "ArrowDown",
        Key::ArrowLeft => "ArrowLeft",
        Key::ArrowRight => "ArrowRight",
        Key::Minus => "-",
        other => other.symbol_or_name(),
    }
    .to_owned()
}

/// The chord a key press makes. On macOS the command key is `Ctrl` in a chord, and a press with
/// the real Control key held makes none (it belongs to the text system and the terminal).
#[must_use]
pub fn chord(key: Key, m: Modifiers) -> Option<Chord> {
    if mac() && m.ctrl {
        return None;
    }
    Some(Chord { ctrl: m.command, shift: m.shift, alt: m.alt, key: token(key) })
}

/// Whether a chord names a key this app can see.
#[must_use]
pub fn known(chord: &Chord) -> bool {
    Key::from_name(&chord.key).is_some()
}

/// A chord as the platform writes it: `Cmd+Shift+T` on macOS, `Ctrl+Shift+T` elsewhere.
#[must_use]
pub fn display(chord: &Chord) -> String {
    let text = chord.to_string();
    if mac() { text.replacen("Ctrl+", "Cmd+", 1) } else { text }
}

/// The first chord bound to `id`, for a menu item's shortcut text ("" when unbound).
#[must_use]
pub fn label(ctx: &Context, id: &str) -> String {
    get(ctx).chords(id).first().map(display).unwrap_or_default()
}

/// The chords the platform delivers as clipboard events rather than key presses.
fn clipboard_chord(event: &Event) -> Option<Chord> {
    let key = match event {
        Event::Copy => "C",
        Event::Cut => "X",
        Event::Paste(_) => "V",
        _ => return None,
    };
    Some(Chord { ctrl: true, shift: false, alt: false, key: key.to_owned() })
}

/// Take this frame's press of a chord bound to `id`, if there is one, so nothing else sees it.
/// `scope` is where the keyboard is; `None` for a command live everywhere.
pub fn take(ctx: &Context, id: &str, scope: Option<Scope>) -> bool {
    let Some(command) = command(id) else { return false };
    if scope.is_some_and(|s| !command.live_in(s)) {
        return false;
    }
    let map = get(ctx);
    let chords = map.chords(id);
    if chords.is_empty() {
        return false;
    }
    ctx.input_mut(|i| {
        let hit = i.events.iter().position(|e| {
            let pressed = match e {
                Event::Key { key, pressed: true, modifiers, .. } => chord(*key, *modifiers),
                other => clipboard_chord(other),
            };
            pressed.is_some_and(|c| chords.contains(&c))
        });
        let Some(at) = hit else { return false };
        let event = i.events.remove(at);
        // A plain or shifted key also typed a character; that goes too.
        if let Event::Key { modifiers, .. } = event
            && !modifiers.command
            && !modifiers.alt
            && matches!(i.events.get(at), Some(Event::Text(_)))
        {
            i.events.remove(at);
        }
        true
    })
}

/// The command a key press runs where `scope` has the keyboard.
#[must_use]
pub fn lookup(ctx: &Context, key: Key, m: Modifiers, scope: Scope) -> Option<&'static str> {
    chord(key, m).and_then(|c| get(ctx).lookup(&c, scope))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_presses_make_the_chords_the_file_writes() {
        let ctrl_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        assert_eq!(chord(Key::T, ctrl_shift).unwrap().to_string(), "Ctrl+Shift+T");
        assert_eq!(chord(Key::Minus, Modifiers::COMMAND).unwrap(), Chord::parse("Ctrl+-").unwrap());
        assert_eq!(chord(Key::Equals, Modifiers::COMMAND).unwrap(), Chord::parse("Ctrl+=").unwrap());
        assert_eq!(chord(Key::ArrowUp, Modifiers::ALT).unwrap(), Chord::parse("Alt+Up").unwrap());
        assert_eq!(chord(Key::F3, Modifiers::SHIFT).unwrap(), Chord::parse("shift+f3").unwrap());
        assert_eq!(chord(Key::Comma, Modifiers::COMMAND).unwrap(), Chord::parse("Ctrl+,").unwrap());
        for c in throng_core::keymap::COMMANDS {
            for chord in c.defaults(mac()) {
                assert!(known(&chord), "{}: {chord}", c.id);
            }
        }
    }

    #[test]
    fn a_taken_press_is_gone_and_a_command_is_not_taken_outside_its_scope() {
        let ctx = Context::default();
        let press =
            |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        let input = egui::RawInput {
            events: vec![press(Key::G, Modifiers::COMMAND), press(Key::F3, Modifiers::NONE)],
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            let ctx = ui.ctx();
            assert!(!take(ctx, "navigate.gotoLine", Some(Scope::Terminal)), "Go To Line is an editor's");
            assert!(take(ctx, "navigate.gotoLine", Some(Scope::Editor)));
            assert!(!take(ctx, "navigate.gotoLine", Some(Scope::Editor)), "once");
            assert!(take(ctx, "search.findNext", Some(Scope::Terminal)));
            assert!(ctx.input(|i| i.events.is_empty()));
        });
        output.textures_delta.clear();
    }
}
