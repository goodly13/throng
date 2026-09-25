//! Keyboard → bytes, the way xterm encodes it.
//!
//! Terminal keys belong to the terminal (Principle IV): Ctrl+C/D/Z/A/E/W/U/K/R/L/Q always reach the
//! shell. Copy and paste use Ctrl+Shift+C/V (Cmd+C/V on macOS); plain Ctrl+C copies only when there
//! is a selection, and otherwise interrupts as the shell expects.

use egui::{Key, Modifiers};

/// Whether Alt acts as Meta (prefixing ESC). On macOS Option composes characters instead.
#[must_use]
pub fn alt_is_meta() -> bool {
    !cfg!(target_os = "macos")
}

/// xterm's modifier parameter: 1 + shift + 2·alt + 4·ctrl.
fn modifier_param(m: Modifiers) -> u8 {
    1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.ctrl)
}

/// Encode a key press, or `None` when the key produces text (handled by the text event) or nothing.
#[must_use]
pub fn encode(key: Key, m: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let param = modifier_param(m);
    let esc = |body: &str| format!("\x1b{body}").into_bytes();
    let cursor = |letter: char| {
        if param > 1 {
            esc(&format!("[1;{param}{letter}"))
        } else if app_cursor {
            esc(&format!("O{letter}"))
        } else {
            esc(&format!("[{letter}"))
        }
    };
    let tilde =
        |code: u8| if param > 1 { esc(&format!("[{code};{param}~")) } else { esc(&format!("[{code}~")) };
    let meta = |bytes: &[u8]| {
        let mut out = Vec::with_capacity(bytes.len() + 1);
        if m.alt && alt_is_meta() {
            out.push(0x1b);
        }
        out.extend_from_slice(bytes);
        out
    };
    let bytes = match key {
        Key::Enter => meta(b"\r"),
        Key::Tab if m.shift => esc("[Z"),
        Key::Tab => meta(b"\t"),
        Key::Backspace if m.ctrl => meta(b"\x08"),
        Key::Backspace => meta(b"\x7f"),
        Key::Escape => meta(b"\x1b"),
        Key::ArrowUp => cursor('A'),
        Key::ArrowDown => cursor('B'),
        Key::ArrowRight => cursor('C'),
        Key::ArrowLeft => cursor('D'),
        Key::Home => cursor('H'),
        Key::End => cursor('F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F1 | Key::F2 | Key::F3 | Key::F4 => {
            let letter = match key {
                Key::F1 => 'P',
                Key::F2 => 'Q',
                Key::F3 => 'R',
                _ => 'S',
            };
            if param > 1 { esc(&format!("[1;{param}{letter}")) } else { esc(&format!("O{letter}")) }
        }
        Key::F5 => tilde(15),
        Key::F6 => tilde(17),
        Key::F7 => tilde(18),
        Key::F8 => tilde(19),
        Key::F9 => tilde(20),
        Key::F10 => tilde(21),
        Key::F11 => tilde(23),
        Key::F12 => tilde(24),
        _ if m.ctrl && !m.shift => return control(key).map(|b| meta(&[b])),
        _ if m.alt && alt_is_meta() && !m.ctrl => {
            let c = printable(key, m.shift)?;
            meta(&[c])
        }
        _ => return None,
    };
    Some(bytes)
}

/// The kitty keyboard protocol's enhancements a program switched on (with `CSI > flags u`; the
/// emulator keeps the stack). throng encodes the first, fourth and fifth: disambiguation, which is
/// what lets a program tell Shift+Enter from Enter, reporting every key as an escape code, and the
/// text a key typed alongside it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Kitty {
    pub disambiguate: bool,
    pub all_keys: bool,
    pub associated_text: bool,
}

impl Kitty {
    #[must_use]
    pub fn on(self) -> bool {
        self.disambiguate || self.all_keys
    }
}

/// kitty's modifier parameter: 1 + shift + 2·alt + 4·ctrl.
fn kitty_mods(m: Modifiers) -> u8 {
    1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.ctrl)
}

/// `CSI code ; mods u`, with the modifiers left out when there are none.
#[must_use]
pub fn csi_u(code: u32, m: Modifiers) -> Vec<u8> {
    let mods = kitty_mods(m);
    if mods > 1 { format!("\x1b[{code};{mods}u").into_bytes() } else { format!("\x1b[{code}u").into_bytes() }
}

/// `CSI code ; mods ; text u`: a key with the text it typed as code points, which is how a program
/// that asked for every key as an escape code learns what was typed (`CSI 32;;32u` is a Space).
#[must_use]
pub fn csi_u_text(code: u32, m: Modifiers, text: &str) -> Vec<u8> {
    let mods = kitty_mods(m);
    let mods = if mods > 1 { mods.to_string() } else { String::new() };
    let text: Vec<String> = text.chars().map(|c| u32::from(c).to_string()).collect();
    format!("\x1b[{code};{mods};{}u", text.join(":")).into_bytes()
}

/// A key press under the kitty protocol, or `None` where the legacy encoding stands (arrows, the
/// function keys, and plain text while only disambiguation is on).
#[must_use]
pub fn encode_kitty(key: Key, m: Modifiers, kitty: Kitty) -> Option<Vec<u8>> {
    encode_kitty_typed(key, m, kitty, None)
}

/// [`encode_kitty`] for a press that typed `text`, which rides along when the program asked for it.
#[must_use]
pub fn encode_kitty_typed(key: Key, m: Modifiers, kitty: Kitty, text: Option<&str>) -> Option<Vec<u8>> {
    let csi_u = |code: u32, m: Modifiers| match text.filter(|_| kitty.all_keys && kitty.associated_text) {
        Some(text) => csi_u_text(code, m, text),
        None => csi_u(code, m),
    };
    if !kitty.on() {
        return None;
    }
    let modified = m.shift || m.alt || m.ctrl;
    let code = match key {
        // Escape is always reported, so a lone Esc is never mistaken for the start of a sequence.
        Key::Escape => return Some(csi_u(27, m)),
        Key::Enter => 13,
        Key::Tab => 9,
        Key::Backspace => 127,
        Key::Space if m.ctrl || m.alt || kitty.all_keys => 32,
        _ => {
            let c = printable(key, false)?;
            // Ctrl and Alt chords are what legacy encoding makes ambiguous; with every key
            // reported, plain letters are escape codes too.
            if !(m.ctrl || m.alt || kitty.all_keys) {
                return None;
            }
            return Some(csi_u(u32::from(c), m));
        }
    };
    (modified || kitty.all_keys).then(|| csi_u(code, m))
}

/// The control code for Ctrl+`key`.
fn control(key: Key) -> Option<u8> {
    let name = key.name();
    if name.len() == 1 {
        let c = name.as_bytes()[0];
        if c.is_ascii_alphabetic() {
            return Some(c.to_ascii_lowercase() & 0x1f);
        }
    }
    Some(match key {
        Key::Space | Key::Num2 => 0x00,
        Key::OpenBracket | Key::Num3 => 0x1b,
        Key::Backslash | Key::Num4 => 0x1c,
        Key::CloseBracket | Key::Num5 => 0x1d,
        Key::Num6 => 0x1e,
        Key::Minus | Key::Slash | Key::Num7 => 0x1f,
        Key::Num8 => 0x7f,
        _ => return None,
    })
}

/// The ASCII byte a letter or digit key types.
fn printable(key: Key, shift: bool) -> Option<u8> {
    let name = key.name();
    if name.len() != 1 {
        return None;
    }
    let c = name.as_bytes()[0];
    if c.is_ascii_alphabetic() {
        Some(if shift { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() })
    } else if c.is_ascii_digit() {
        Some(c)
    } else {
        None
    }
}

/// Text to send for a paste: newlines become carriage returns, and bracketed-paste mode wraps it so
/// the shell can tell pasted text from typed text. Escape sequences inside the paste are neutralised
/// so pasted text can never end the bracket early and execute.
#[must_use]
pub fn paste(text: &str, bracketed: bool) -> Vec<u8> {
    let normalised = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        let cleaned = normalised.replace('\x1b', "");
        format!("\x1b[200~{cleaned}\x1b[201~").into_bytes()
    } else {
        normalised.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers::NONE;

    fn ctrl() -> Modifiers {
        Modifiers { ctrl: true, ..Modifiers::NONE }
    }

    #[test]
    fn control_letters_reach_the_shell() {
        assert_eq!(encode(Key::C, ctrl(), false), Some(vec![0x03]));
        assert_eq!(encode(Key::D, ctrl(), false), Some(vec![0x04]));
        assert_eq!(encode(Key::W, ctrl(), false), Some(vec![0x17]));
        assert_eq!(encode(Key::OpenBracket, ctrl(), false), Some(vec![0x1b]));
        assert_eq!(encode(Key::Space, ctrl(), false), Some(vec![0x00]));
    }

    #[test]
    fn cursor_keys_follow_application_mode_and_modifiers() {
        assert_eq!(encode(Key::ArrowUp, NONE, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode(Key::ArrowUp, NONE, true), Some(b"\x1bOA".to_vec()));
        assert_eq!(encode(Key::ArrowLeft, ctrl(), true), Some(b"\x1b[1;5D".to_vec()));
        let shift = Modifiers { shift: true, ..NONE };
        assert_eq!(encode(Key::End, shift, false), Some(b"\x1b[1;2F".to_vec()));
    }

    #[test]
    fn editing_and_function_keys() {
        assert_eq!(encode(Key::Enter, NONE, false), Some(b"\r".to_vec()));
        assert_eq!(encode(Key::Backspace, NONE, false), Some(b"\x7f".to_vec()));
        assert_eq!(encode(Key::Tab, Modifiers { shift: true, ..NONE }, false), Some(b"\x1b[Z".to_vec()));
        assert_eq!(encode(Key::Delete, NONE, false), Some(b"\x1b[3~".to_vec()));
        assert_eq!(encode(Key::PageUp, ctrl(), false), Some(b"\x1b[5;5~".to_vec()));
        assert_eq!(encode(Key::F1, NONE, false), Some(b"\x1bOP".to_vec()));
        assert_eq!(encode(Key::F12, NONE, false), Some(b"\x1b[24~".to_vec()));
    }

    #[test]
    fn plain_letters_are_left_to_text_events() {
        assert_eq!(encode(Key::A, NONE, false), None);
    }

    #[test]
    fn alt_is_meta_off_macos() {
        let alt = Modifiers { alt: true, ..NONE };
        let expected = if alt_is_meta() { Some(b"\x1bb".to_vec()) } else { None };
        assert_eq!(encode(Key::B, alt, false), expected);
    }

    #[test]
    fn the_kitty_protocol_tells_modified_keys_apart_and_leaves_plain_ones_alone() {
        let on = Kitty { disambiguate: true, ..Kitty::default() };
        let shift = Modifiers::SHIFT;
        let ctrl = Modifiers { ctrl: true, command: !cfg!(target_os = "macos"), ..Modifiers::NONE };
        assert_eq!(encode_kitty(Key::Enter, shift, on), Some(b"\x1b[13;2u".to_vec()), "Shift+Enter");
        assert_eq!(
            encode_kitty(Key::Enter, Modifiers::NONE, on),
            None,
            "plain Enter stays a carriage return"
        );
        assert_eq!(encode_kitty(Key::Escape, Modifiers::NONE, on), Some(b"\x1b[27u".to_vec()));
        assert_eq!(encode_kitty(Key::A, ctrl, on), Some(b"\x1b[97;5u".to_vec()), "Ctrl+A");
        assert_eq!(encode_kitty(Key::A, Modifiers::NONE, on), None, "text stays text");
        assert_eq!(encode_kitty(Key::ArrowUp, shift, on), None, "arrows keep their CSI form");
        let all = Kitty { disambiguate: true, all_keys: true, ..Kitty::default() };
        assert_eq!(encode_kitty(Key::A, Modifiers::NONE, all), Some(b"\x1b[97u".to_vec()));
        assert_eq!(encode_kitty(Key::Enter, Modifiers::NONE, all), Some(b"\x1b[13u".to_vec()));
        assert_eq!(encode_kitty(Key::Enter, shift, Kitty::default()), None, "off: legacy");
    }

    #[test]
    fn pastes_are_bracketed_and_cannot_break_out() {
        assert_eq!(paste("a\nb", false), b"a\rb".to_vec());
        assert_eq!(paste("x\x1b[201~rm -rf /\n", true), b"\x1b[200~x[201~rm -rf /\r\x1b[201~".to_vec());
    }
}
