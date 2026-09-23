//! Terminal colours.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use egui::Color32;

/// A terminal palette.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub foreground: Color32,
    pub background: Color32,
    pub cursor: Color32,
    pub selection: Color32,
    pub ansi: [Color32; 16],
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Palette {
    #[must_use]
    pub fn dark() -> Self {
        Self {
            foreground: rgb(0xd4d4d4),
            background: rgb(0x16181c),
            cursor: rgb(0xe6e6e6),
            selection: Color32::from_rgba_unmultiplied(0x5a, 0x8b, 0xd6, 90),
            ansi: [
                rgb(0x1e1e1e),
                rgb(0xe5534b),
                rgb(0x57ab5a),
                rgb(0xc69026),
                rgb(0x539bf5),
                rgb(0xb083f0),
                rgb(0x39c5cf),
                rgb(0xcdd9e5),
                rgb(0x636e7b),
                rgb(0xff938a),
                rgb(0x6bc46d),
                rgb(0xdaaa3f),
                rgb(0x6cb6ff),
                rgb(0xdcbdfb),
                rgb(0x56d4dd),
                rgb(0xf0f6fc),
            ],
        }
    }

    #[must_use]
    pub fn light() -> Self {
        Self {
            foreground: rgb(0x24292f),
            background: rgb(0xfbfbfb),
            cursor: rgb(0x24292f),
            selection: Color32::from_rgba_unmultiplied(0x54, 0xae, 0xff, 80),
            ansi: [
                rgb(0x24292f),
                rgb(0xcf222e),
                rgb(0x116329),
                rgb(0x4d2d00),
                rgb(0x0969da),
                rgb(0x8250df),
                rgb(0x1b7c83),
                rgb(0x6e7781),
                rgb(0x57606a),
                rgb(0xa40e26),
                rgb(0x1a7f37),
                rgb(0x633c01),
                rgb(0x218bff),
                rgb(0xa475f9),
                rgb(0x3192aa),
                rgb(0x8c959f),
            ],
        }
    }

    /// Resolve an emulator colour. Colours the program redefined (OSC 4/10/11) win.
    #[must_use]
    pub fn resolve(&self, color: Color, overrides: &Colors) -> Color32 {
        match color {
            Color::Spec(Rgb { r, g, b }) => Color32::from_rgb(r, g, b),
            Color::Indexed(index) => {
                if let Some(Rgb { r, g, b }) = overrides[usize::from(index)] {
                    return Color32::from_rgb(r, g, b);
                }
                self.indexed(index)
            }
            Color::Named(named) => {
                if let Some(Rgb { r, g, b }) = overrides[named as usize] {
                    return Color32::from_rgb(r, g, b);
                }
                self.named(named)
            }
        }
    }

    fn named(&self, named: NamedColor) -> Color32 {
        use NamedColor as N;
        match named {
            N::Foreground | N::BrightForeground => self.foreground,
            N::Background => self.background,
            N::Cursor => self.cursor,
            N::DimForeground => dim(self.foreground, self.background),
            N::DimBlack => dim(self.ansi[0], self.background),
            N::DimRed => dim(self.ansi[1], self.background),
            N::DimGreen => dim(self.ansi[2], self.background),
            N::DimYellow => dim(self.ansi[3], self.background),
            N::DimBlue => dim(self.ansi[4], self.background),
            N::DimMagenta => dim(self.ansi[5], self.background),
            N::DimCyan => dim(self.ansi[6], self.background),
            N::DimWhite => dim(self.ansi[7], self.background),
            other => {
                let index = other as usize;
                if index < 16 { self.ansi[index] } else { self.foreground }
            }
        }
    }

    /// The xterm 256-colour table.
    #[must_use]
    pub fn indexed(&self, index: u8) -> Color32 {
        match index {
            0..=15 => self.ansi[usize::from(index)],
            16..=231 => {
                let i = index - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Color32::from_rgb(level(i / 36), level((i / 6) % 6), level(i % 6))
            }
            232..=255 => {
                let v = 8 + (index - 232) * 10;
                Color32::from_rgb(v, v, v)
            }
        }
    }

    /// The bright variant of a basic colour, for bold text.
    #[must_use]
    pub fn brighten(&self, color: Color) -> Color {
        match color {
            Color::Named(named) if (named as usize) < 8 => Color::Indexed(named as u8 + 8),
            Color::Indexed(index) if index < 8 => Color::Indexed(index + 8),
            other => other,
        }
    }
}

/// Blend towards the background (the SGR "dim" attribute).
#[must_use]
pub fn dim(color: Color32, background: Color32) -> Color32 {
    let mix = |a: u8, b: u8| ((u16::from(a) * 2 + u16::from(b)) / 3) as u8;
    Color32::from_rgb(
        mix(color.r(), background.r()),
        mix(color.g(), background.g()),
        mix(color.b(), background.b()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_256_colour_cube_and_ramp() {
        let p = Palette::dark();
        assert_eq!(p.indexed(16), Color32::from_rgb(0, 0, 0));
        assert_eq!(p.indexed(231), Color32::from_rgb(255, 255, 255));
        assert_eq!(p.indexed(196), Color32::from_rgb(255, 0, 0));
        assert_eq!(p.indexed(232), Color32::from_rgb(8, 8, 8));
        assert_eq!(p.indexed(255), Color32::from_rgb(238, 238, 238));
        assert_eq!(p.indexed(1), p.ansi[1]);
    }

    #[test]
    fn overrides_win() {
        let p = Palette::dark();
        let mut overrides = Colors::default();
        overrides[1] = Some(Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(p.resolve(Color::Indexed(1), &overrides), Color32::from_rgb(1, 2, 3));
        assert_eq!(p.resolve(Color::Named(NamedColor::Red), &overrides), Color32::from_rgb(1, 2, 3));
        assert_eq!(p.resolve(Color::Spec(Rgb { r: 9, g: 9, b: 9 }), &overrides), Color32::from_rgb(9, 9, 9));
    }

    #[test]
    fn bold_brightens_basic_colours_only() {
        let p = Palette::dark();
        assert_eq!(p.brighten(Color::Named(NamedColor::Red)), Color::Indexed(9));
        assert_eq!(p.brighten(Color::Indexed(200)), Color::Indexed(200));
    }
}
