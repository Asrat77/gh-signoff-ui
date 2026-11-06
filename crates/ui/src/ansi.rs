//! Reading ANSI colour codes in command output.
//!
//! Test runners and linters colour their output. [`parse`] turns a line
//! into spans with the SGR colour and weight it asked for, and drops every
//! other escape sequence (cursor moves, erase, titles) so nothing strange
//! reaches the log view.

use egui::Color32;

/// One of the sixteen terminal colours, or a 256-colour or true colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    /// 0 to 7, the normal colours (black, red, green, yellow, blue,
    /// magenta, cyan, white); 8 to 15, their bright versions; 16 to 255,
    /// the 256-colour cube and greys.
    Indexed(u8),
    /// A 24-bit colour.
    Rgb(u8, u8, u8),
}

/// The style a span of text was printed in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style {
    /// The foreground colour, or the default.
    pub fg: Option<Color>,
    /// Bold or bright.
    pub bold: bool,
    /// Faint.
    pub dim: bool,
}

/// A run of text in one style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    /// The text, without escape codes.
    pub text: String,
    /// How it was printed.
    pub style: Style,
}

/// Splits `line` into styled spans, starting from `style` (the style the
/// previous line ended in), and returns the style it ends in.
#[must_use]
pub fn parse(line: &str, mut style: Style) -> (Vec<Span>, Style) {
    let mut spans: Vec<Span> = Vec::new();
    let mut text = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if c != '\r' {
                text.push(c);
            }
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                let mut params = String::new();
                let mut command = None;
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        command = Some(c);
                        break;
                    }
                    params.push(c);
                }
                if command == Some('m') {
                    flush(&mut spans, &mut text, style);
                    style = apply_sgr(style, &params);
                }
            }
            Some(']') => {
                // An operating system command, such as a window title:
                // skip to the bell or string terminator.
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    flush(&mut spans, &mut text, style);
    (spans, style)
}

/// The text of `line` with every escape code removed.
#[must_use]
pub fn strip(line: &str) -> String {
    parse(line, Style::default()).0.into_iter().map(|span| span.text).collect()
}

fn flush(spans: &mut Vec<Span>, text: &mut String, style: Style) {
    if text.is_empty() {
        return;
    }
    let text = std::mem::take(text);
    match spans.last_mut() {
        Some(last) if last.style == style => last.text.push_str(&text),
        _ => spans.push(Span { text, style }),
    }
}

fn apply_sgr(mut style: Style, params: &str) -> Style {
    let codes: Vec<u16> = if params.is_empty() {
        vec![0]
    } else {
        params.split([';', ':']).map(|code| code.parse().unwrap_or(0)).collect()
    };
    let mut codes = codes.into_iter();
    while let Some(code) = codes.next() {
        match code {
            0 => style = Style::default(),
            1 => style.bold = true,
            2 => style.dim = true,
            22 => {
                style.bold = false;
                style.dim = false;
            }
            30..=37 => style.fg = Some(Color::Indexed(index(code - 30))),
            90..=97 => style.fg = Some(Color::Indexed(index(code - 90 + 8))),
            39 => style.fg = None,
            38 => match codes.next() {
                Some(5) => {
                    if let Some(n) = codes.next() {
                        style.fg = Some(Color::Indexed(index(n)));
                    }
                }
                Some(2) => {
                    let mut channel = || index(codes.next().unwrap_or(0));
                    style.fg = Some(Color::Rgb(channel(), channel(), channel()));
                }
                _ => {}
            },
            // Backgrounds, underline, italics and the rest are ignored:
            // the log view keeps one background.
            _ => {}
        }
    }
    style
}

fn index(value: u16) -> u8 {
    u8::try_from(value).unwrap_or(u8::MAX)
}

/// The colour to draw `color` with on the dark terminal panel.
#[must_use]
pub fn to_color32(color: Color) -> Color32 {
    const BASE: [u32; 16] = [
        0x5A5A60, 0xFF6B5B, 0x4ADE80, 0xFFB224, 0x6AA8FF, 0xD58CFF, 0x5FD7E0, 0xD6D6DA, //
        0x86868E, 0xFF8A7A, 0x7AF0A4, 0xFFD06A, 0x93C2FF, 0xE5B0FF, 0x8AEBF2, 0xFFFFFF,
    ];
    let rgb = |value: u32| {
        let [_, r, g, b] = value.to_be_bytes();
        Color32::from_rgb(r, g, b)
    };
    match color {
        Color::Indexed(n @ 0..=15) => rgb(BASE[usize::from(n)]),
        Color::Indexed(n @ 16..=231) => {
            let n = n - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color32::from_rgb(level(n / 36), level((n / 6) % 6), level(n % 6))
        }
        Color::Indexed(n) => {
            let grey = 8 + (n - 232) * 10;
            Color32::from_rgb(grey, grey, grey)
        }
        Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn red() -> Style {
        Style { fg: Some(Color::Indexed(1)), ..Style::default() }
    }

    #[test]
    fn plain_text_is_one_span() {
        let (spans, _) = parse("cargo test", Style::default());
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "cargo test");
    }

    #[test]
    fn colours_split_spans() {
        let (spans, end) = parse("ok \u{1b}[31mFAILED\u{1b}[0m done", Style::default());
        let texts: Vec<_> = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["ok ", "FAILED", " done"]);
        assert_eq!(spans[1].style, red());
        assert_eq!(end, Style::default());
    }

    #[test]
    fn style_carries_over_to_the_next_line() {
        let (_, end) = parse("\u{1b}[1;31merror:", Style::default());
        assert!(end.bold);
        let (spans, _) = parse("  details", end);
        assert_eq!(spans[0].style.fg, Some(Color::Indexed(1)));
    }

    #[test]
    fn extended_colours_are_read() {
        let (spans, _) = parse("\u{1b}[38;5;208mx\u{1b}[38;2;1;2;3my", Style::default());
        assert_eq!(spans[0].style.fg, Some(Color::Indexed(208)));
        assert_eq!(spans[1].style.fg, Some(Color::Rgb(1, 2, 3)));
    }

    #[test]
    fn other_escapes_are_dropped() {
        assert_eq!(strip("\u{1b}[2K\u{1b}[1Gloading\r"), "loading");
        assert_eq!(strip("\u{1b}]0;title\u{7}after"), "after");
        assert_eq!(strip("bright \u{1b}[92mgreen"), "bright green");
    }

    #[test]
    fn a_truncated_escape_does_not_panic() {
        assert_eq!(strip("text\u{1b}["), "text");
        assert_eq!(strip("text\u{1b}"), "text");
    }

    #[test]
    fn every_index_maps_to_a_colour() {
        for n in 0..=u8::MAX {
            let _ = to_color32(Color::Indexed(n));
        }
    }
}
