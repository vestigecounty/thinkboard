//! Live Markdown styling for the node text editor.
//!
//! Obsidian's default editing mode keeps the raw markers visible but styles the
//! content as you type (bold looks bold, code is highlighted, headings are
//! colored). We get the same by feeding iced's `text_editor` a [`Highlighter`]
//! that syntax-highlights the source line by line: because the styling is
//! applied to the *source* text (not a rendered transform), the caret and
//! selection map 1:1 and stay correct, and iced keeps native text input.
//!
//! Per-range font size isn't expressible through iced's highlighter, so
//! headings are colored and bold but not enlarged.

use std::ops::Range;

use iced::Font;
use iced::advanced::text::highlighter::{self, Highlighter};
use iced::font::{Family, Stretch, Style as FontStyle, Weight};

use crate::theme::Theme as ColorTheme;

/// Resolved colors + font families the highlighter styles with; carried as the
/// editor's highlighter settings (so it re-highlights if the theme changes).
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    marker: iced::Color,
    bold: iced::Color,
    italic: iced::Color,
    code: iced::Color,
    link: iced::Color,
    heading: [iced::Color; 6],
    font_text: Option<&'static str>,
    font_mono: Option<&'static str>,
}

impl Palette {
    pub fn from_theme(theme: &ColorTheme) -> Self {
        Palette {
            // Dim the punctuation so content stands out, like Obsidian.
            marker: iced::Color { a: 0.4, ..theme.text },
            bold: theme.bold,
            italic: theme.italic,
            code: theme.code,
            link: theme.link,
            heading: theme.headings,
            font_text: theme.font_text,
            font_mono: theme.font_mono,
        }
    }

    /// The editor's base font (unstyled text), matching read-mode's body font.
    pub fn base_font(&self) -> Font {
        text_font(self.font_text, Weight::Normal, FontStyle::Normal)
    }
}

/// A resolved highlight: a color and an optional font override for one range.
#[derive(Debug, Clone)]
pub struct Style {
    color: Option<iced::Color>,
    font: Option<Font>,
}

/// Maps a resolved [`Style`] to iced's highlight format. A plain `fn` (iced's
/// requirement); the style already carries the color/font, so the theme arg is
/// unused.
pub fn to_format(style: &Style, _theme: &iced::Theme) -> highlighter::Format<Font> {
    highlighter::Format {
        color: style.color,
        font: style.font,
    }
}

fn text_font(family: Option<&'static str>, weight: Weight, style: FontStyle) -> Font {
    Font {
        family: family.map(Family::Name).unwrap_or(Family::Name(crate::platform::UI_FONT_NAME)),
        weight,
        stretch: Stretch::Normal,
        style,
    }
}

pub struct MdHighlighter {
    palette: Palette,
    line: usize,
    marker: Style,
    bold: Style,
    italic: Style,
    code: Style,
    link: Style,
    heading: [Style; 6],
}

impl MdHighlighter {
    fn build(palette: Palette) -> Self {
        let font = |w, s| Some(text_font(palette.font_text, w, s));
        let mono = Some(Font {
            family: palette.font_mono.map(Family::Name).unwrap_or(Family::Monospace),
            weight: Weight::Normal,
            stretch: Stretch::Normal,
            style: FontStyle::Normal,
        });
        let color = |c| Some(c);
        let heading = std::array::from_fn(|i| Style {
            color: color(palette.heading[i]),
            font: font(Weight::Bold, FontStyle::Normal),
        });
        MdHighlighter {
            line: 0,
            marker: Style { color: color(palette.marker), font: None },
            bold: Style { color: color(palette.bold), font: font(Weight::Bold, FontStyle::Normal) },
            italic: Style {
                color: color(palette.italic),
                font: font(Weight::Normal, FontStyle::Italic),
            },
            code: Style { color: color(palette.code), font: mono },
            link: Style { color: color(palette.link), font: None },
            heading,
            palette,
        }
    }

    /// Emits `(range, style)` spans for one source line.
    fn scan(&self, line: &str, out: &mut Vec<(Range<usize>, Style)>) {
        let bytes = line.as_bytes();

        // Heading: `#`..`#` (1–6) then a space. Marker (hashes + space) dimmed,
        // the rest colored/bold.
        if let Some((marker_end, level)) = heading(bytes) {
            out.push((0..marker_end, self.marker.clone()));
            if marker_end < line.len() {
                out.push((marker_end..line.len(), self.heading[level - 1].clone()));
            }
            return;
        }

        let n = bytes.len();
        let mut i = 0;
        while i < n {
            match bytes[i] {
                b'`' => {
                    if let Some(close) = find(bytes, i + 1, |b| b == b'`') {
                        self.wrap(out, i, 1, close, 1, &self.code);
                        i = close + 1;
                        continue;
                    }
                }
                c @ (b'*' | b'_') if i + 1 < n && bytes[i + 1] == c => {
                    if let Some(close) = find_run(bytes, i + 2, c, 2) {
                        self.wrap(out, i, 2, close, 2, &self.bold);
                        i = close + 2;
                        continue;
                    }
                }
                c @ (b'*' | b'_') => {
                    if let Some(close) = find_single(bytes, i + 1, c) {
                        self.wrap(out, i, 1, close, 1, &self.italic);
                        i = close + 1;
                        continue;
                    }
                }
                b'[' => {
                    if let Some((text_end, link_end)) = link(bytes, i) {
                        out.push((i..i + 1, self.marker.clone())); // [
                        if text_end > i + 1 {
                            out.push((i + 1..text_end, self.link.clone()));
                        }
                        out.push((text_end..link_end, self.marker.clone())); // ](url)
                        i = link_end;
                        continue;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    /// Emits the opening marker, the inner content, and the closing marker for a
    /// span delimited by `open_len`/`close_len` markers.
    fn wrap(
        &self,
        out: &mut Vec<(Range<usize>, Style)>,
        open: usize,
        open_len: usize,
        close: usize,
        close_len: usize,
        inner: &Style,
    ) {
        out.push((open..open + open_len, self.marker.clone()));
        if close > open + open_len {
            out.push((open + open_len..close, inner.clone()));
        }
        out.push((close..close + close_len, self.marker.clone()));
    }
}

impl Highlighter for MdHighlighter {
    type Settings = Palette;
    type Highlight = Style;
    type Iterator<'a> = std::vec::IntoIter<(Range<usize>, Style)>;

    fn new(settings: &Palette) -> Self {
        Self::build(settings.clone())
    }

    fn update(&mut self, settings: &Palette) {
        if settings != &self.palette {
            *self = Self::build(settings.clone());
        }
    }

    /// Keeps the *earliest* line changed since the last highlight pass, as the
    /// trait asks of `current_line`. The editor calls this from every layout
    /// but highlights only when it draws, so two edits on different lines
    /// between frames arrive as two calls — and taking the second outright
    /// skipped the first: a heading typed, then a body line under it before
    /// the next frame, stayed unstyled until it was edited again.
    fn change_line(&mut self, line: usize) {
        self.line = self.line.min(line);
    }

    fn highlight_line(&mut self, line: &str) -> Self::Iterator<'_> {
        let mut out = Vec::new();
        self.scan(line, &mut out);
        self.line += 1;
        out.into_iter()
    }

    fn current_line(&self) -> usize {
        self.line
    }
}

/// Marker length + level of a heading line (`#`..`#` then a space), or `None`.
fn heading(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < 3 && bytes.get(i) == Some(&b' ') {
        i += 1;
    }
    let start = i;
    while bytes.get(i) == Some(&b'#') {
        i += 1;
    }
    let level = i - start;
    if (1..=6).contains(&level) && bytes.get(i) == Some(&b' ') {
        Some((i + 1, level)) // include the space in the marker
    } else {
        None
    }
}

fn find(bytes: &[u8], from: usize, pred: impl Fn(u8) -> bool) -> Option<usize> {
    (from..bytes.len()).find(|&i| pred(bytes[i]))
}

/// First single `c` at/after `from` that isn't part of a `cc` run (so `*`
/// doesn't close on the `**` of a bold marker).
fn find_single(bytes: &[u8], from: usize, c: u8) -> Option<usize> {
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == c {
            if bytes.get(i + 1) == Some(&c) {
                i += 2; // skip a doubled run
                continue;
            }
            return Some(i);
        }
        i += 1;
    }
    None
}

/// First run of `len` consecutive `c` at/after `from`.
fn find_run(bytes: &[u8], from: usize, c: u8, len: usize) -> Option<usize> {
    (from..bytes.len().saturating_sub(len - 1))
        .find(|&i| bytes[i..i + len].iter().all(|&b| b == c))
}

/// Metric-changing styling the editor applies to a span of source: bold and
/// italic (weight/slant) and inline code (mono family). Markers, links, and
/// heading hashes keep the base metrics (they're only recolored), so they don't
/// affect wrapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emphasis {
    Bold,
    Italic,
    Mono,
}

/// The metric-changing spans the live editor styles a source line with, in
/// source-byte ranges. Mirrors [`MdHighlighter::scan`] but yields only the
/// inner content whose glyph metrics differ from the base font — so a measurer
/// can reproduce the editor's wrapped height (headings are bolded, not
/// enlarged; see the module note). Plain text yields nothing.
pub fn emphasis_spans(line: &str, out: &mut Vec<(Range<usize>, Emphasis)>) {
    let bytes = line.as_bytes();

    // Heading body is bold (same size as body text in the editor).
    if let Some((marker_end, _level)) = heading(bytes) {
        if marker_end < line.len() {
            out.push((marker_end..line.len(), Emphasis::Bold));
        }
        return;
    }

    let n = bytes.len();
    let mut i = 0;
    while i < n {
        match bytes[i] {
            b'`' => {
                if let Some(close) = find(bytes, i + 1, |b| b == b'`') {
                    if close > i + 1 {
                        out.push((i + 1..close, Emphasis::Mono));
                    }
                    i = close + 1;
                    continue;
                }
            }
            c @ (b'*' | b'_') if i + 1 < n && bytes[i + 1] == c => {
                if let Some(close) = find_run(bytes, i + 2, c, 2) {
                    if close > i + 2 {
                        out.push((i + 2..close, Emphasis::Bold));
                    }
                    i = close + 2;
                    continue;
                }
            }
            c @ (b'*' | b'_') => {
                if let Some(close) = find_single(bytes, i + 1, c) {
                    if close > i + 1 {
                        out.push((i + 1..close, Emphasis::Italic));
                    }
                    i = close + 1;
                    continue;
                }
            }
            b'[' => {
                if let Some((_text_end, link_end)) = link(bytes, i) {
                    i = link_end; // link text keeps base metrics
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
}

/// A `[text](url)` starting at `open` (`bytes[open] == '['`): returns the index
/// of `]` and the index just past `)`.
fn link(bytes: &[u8], open: usize) -> Option<(usize, usize)> {
    let close_bracket = find(bytes, open + 1, |b| b == b']')?;
    if bytes.get(close_bracket + 1) != Some(&b'(') {
        return None;
    }
    let close_paren = find(bytes, close_bracket + 2, |b| b == b')')?;
    Some((close_bracket, close_paren + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(line: &str) -> Vec<Range<usize>> {
        let mut h = MdHighlighter::build(Palette::from_theme(&ColorTheme::default()));
        h.highlight_line(line).map(|(r, _)| r).collect()
    }

    #[test]
    fn bold_splits_markers_and_content() {
        // "a **b** c": markers 2..4 and 5..7, bold content 4..5.
        assert_eq!(ranges("a **b** c"), vec![2..4, 4..5, 5..7]);
    }

    #[test]
    fn heading_marker_then_text() {
        assert_eq!(ranges("# Title"), vec![0..2, 2..7]);
    }

    #[test]
    fn a_later_change_does_not_skip_an_earlier_one() {
        // Three lines highlighted; then, before the next pass, an edit on the
        // first line and another on the third. The pass must start at the
        // first, or the heading on it keeps the styling of what it used to say.
        let mut h = MdHighlighter::build(Palette::from_theme(&ColorTheme::default()));
        for line in ["# Title", "", "body"] {
            let _ = h.highlight_line(line).count();
        }
        h.change_line(0);
        h.change_line(2);
        assert_eq!(h.current_line(), 0);
    }

    #[test]
    fn inline_code_spans() {
        assert_eq!(ranges("`x`"), vec![0..1, 1..2, 2..3]);
    }

    #[test]
    fn lone_italic_is_not_confused_by_bold() {
        assert_eq!(ranges("*i* and **b**"), vec![0..1, 1..2, 2..3, 8..10, 10..11, 11..13]);
    }

    #[test]
    fn link_text_is_styled() {
        // "[x](y)": `[` marker, `x` link, `](y)` marker.
        assert_eq!(ranges("[x](y)"), vec![0..1, 1..2, 2..6]);
    }

    #[test]
    fn plain_text_has_no_spans() {
        assert!(ranges("just a label").is_empty());
    }

    fn emphasis(line: &str) -> Vec<(Range<usize>, Emphasis)> {
        let mut out = Vec::new();
        emphasis_spans(line, &mut out);
        out
    }

    #[test]
    fn emphasis_spans_report_metric_ranges_only() {
        // Bold/italic/code inner content only (markers excluded).
        assert_eq!(emphasis("a **b** c"), vec![(4..5, Emphasis::Bold)]);
        assert_eq!(emphasis("*i* and `x`"), vec![(1..2, Emphasis::Italic), (9..10, Emphasis::Mono)]);
        // Heading body is bold; the hashes are not.
        assert_eq!(emphasis("## Title"), vec![(3..8, Emphasis::Bold)]);
        // Links keep base metrics — nothing emphasized.
        assert!(emphasis("see [x](y) here").is_empty());
        assert!(emphasis("plain text").is_empty());
    }
}
