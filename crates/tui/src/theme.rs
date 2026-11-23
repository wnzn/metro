use std::{
    env,
    io::{self, Write},
};

use crossterm::{
    queue,
    style::{Attribute, Color, SetAttribute, SetBackgroundColor, SetForegroundColor},
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Mono,
    Indexed,
    Rgb,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Strong,
    Rail,
    Brand,
    Selected,
    Ai,
    Info,
    Success,
    Warning,
    Error,
}

struct Swatch(u8, u8, u8, u8);

const BLUE: Swatch = Swatch(0x0A, 0x58, 0xFF, 27);
const NAVY: Swatch = Swatch(0x08, 0x1B, 0x3B, 17);
const CHARCOAL: Swatch = Swatch(0x0B, 0x0F, 0x19, 233);
const SLATE: Swatch = Swatch(0xF8, 0xFA, 0xFF, 231);
const VIOLET: Swatch = Swatch(0x8B, 0x5C, 0xF6, 99);
const TEAL: Swatch = Swatch(0x06, 0xB6, 0xD4, 38);
const GREEN: Swatch = Swatch(0x10, 0xB9, 0x81, 35);
const AMBER: Swatch = Swatch(0xF5, 0x9E, 0x0B, 214);
const RED: Swatch = Swatch(0xEF, 0x44, 0x44, 203);

pub fn dumb_terminal() -> bool {
    env::var("TERM").is_ok_and(|s| s.eq_ignore_ascii_case("dumb"))
}

impl Mode {
    pub fn detect() -> Self {
        Self::capabilities(
            &env::var("TERM").unwrap_or_default().to_ascii_lowercase(),
            &env::var("COLORTERM")
                .unwrap_or_default()
                .to_ascii_lowercase(),
            env::var_os("NO_COLOR").is_some(),
        )
    }

    fn capabilities(term: &str, color_term: &str, no_color: bool) -> Self {
        if no_color || term == "dumb" {
            Self::Mono
        } else if matches!(color_term, "truecolor" | "24bit")
            || term.ends_with("-direct")
            || term.ends_with("-truecolor")
        {
            Self::Rgb
        } else if term.ends_with("-256color") {
            Self::Indexed
        } else {
            // Don't guess what palette entries mean on a limited console.
            Self::Mono
        }
    }

    fn color(self, s: &Swatch) -> Color {
        match self {
            Self::Rgb => Color::Rgb {
                r: s.0,
                g: s.1,
                b: s.2,
            },
            Self::Indexed => Color::AnsiValue(s.3),
            Self::Mono => unreachable!("monochrome does not set color"),
        }
    }

    pub fn begin(self, out: &mut impl Write, tone: Tone) -> io::Result<()> {
        if tone == Tone::Plain {
            return Ok(());
        }
        if self == Self::Mono || tone == Tone::Strong {
            let attr = if matches!(tone, Tone::Selected | Tone::Brand) {
                Attribute::Reverse
            } else {
                Attribute::Bold
            };
            return queue!(out, SetAttribute(attr));
        }
        // Every color is paired. Text on the user's background uses the
        // user's foreground instead, and never assumes a light/dark theme.
        let (fg, bg) = match tone {
            Tone::Rail => (&SLATE, &NAVY),
            Tone::Brand | Tone::Selected => (&SLATE, &BLUE),
            Tone::Ai => (&CHARCOAL, &VIOLET),
            Tone::Info => (&CHARCOAL, &TEAL),
            Tone::Success => (&CHARCOAL, &GREEN),
            Tone::Warning => (&CHARCOAL, &AMBER),
            Tone::Error => (&CHARCOAL, &RED),
            Tone::Plain | Tone::Strong => unreachable!(),
        };
        queue!(
            out,
            SetForegroundColor(self.color(fg)),
            SetBackgroundColor(self.color(bg))
        )?;
        if tone == Tone::Brand {
            queue!(out, SetAttribute(Attribute::Bold))?;
        }
        Ok(())
    }

    pub fn end(self, out: &mut impl Write, tone: Tone) -> io::Result<()> {
        if tone != Tone::Plain {
            queue!(out, SetAttribute(Attribute::Reset))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_capabilities_are_conservative() {
        for (term, color_term, no_color, expected) in [
            ("xterm-256color", "", false, Mode::Indexed),
            ("xterm-256color", "truecolor", false, Mode::Rgb),
            ("xterm-direct", "", false, Mode::Rgb),
            ("xterm", "", false, Mode::Mono),
            ("linux", "", false, Mode::Mono),
            ("dumb", "truecolor", false, Mode::Mono),
            ("xterm-direct", "truecolor", true, Mode::Mono),
        ] {
            assert_eq!(Mode::capabilities(term, color_term, no_color), expected);
        }
    }

    #[test]
    fn paired_colors_meet_text_contrast_in_rgb_and_xterm_palettes() {
        fn light(mode: Mode, swatch: &Swatch) -> f64 {
            let rgb = match mode {
                Mode::Indexed if swatch.3 < 232 => {
                    let n = swatch.3 - 16;
                    let cube = [0, 95, 135, 175, 215, 255];
                    [
                        cube[(n / 36) as usize],
                        cube[(n / 6 % 6) as usize],
                        cube[(n % 6) as usize],
                    ]
                }
                Mode::Indexed => [8 + 10 * (swatch.3 - 232); 3],
                _ => [swatch.0, swatch.1, swatch.2],
            };
            rgb.into_iter()
                .zip([0.2126, 0.7152, 0.0722])
                .map(|(channel, weight)| {
                    let value = f64::from(channel) / 255.0;
                    let linear = if value <= 0.04045 {
                        value / 12.92
                    } else {
                        ((value + 0.055) / 1.055).powf(2.4)
                    };
                    weight * linear
                })
                .sum()
        }

        for mode in [Mode::Rgb, Mode::Indexed] {
            for (fg, bg) in [
                (&SLATE, &NAVY),
                (&SLATE, &BLUE),
                (&CHARCOAL, &VIOLET),
                (&CHARCOAL, &TEAL),
                (&CHARCOAL, &GREEN),
                (&CHARCOAL, &AMBER),
                (&CHARCOAL, &RED),
            ] {
                let (fg, bg) = (light(mode, fg), light(mode, bg));
                assert!((fg.max(bg) + 0.05) / (fg.min(bg) + 0.05) >= 4.5, "{mode:?}");
            }
        }
    }
}
