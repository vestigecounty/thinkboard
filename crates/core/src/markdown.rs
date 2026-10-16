//! Minimal Markdown → styled spans, for rendering node text (read mode).
//!
//! Flattens `pulldown-cmark` events into a linear run of [`Span`]s carrying
//! inline style (bold/italic/code/link) and block context (heading level,
//! list bullets, paragraph breaks). The rasterizer turns each span into a
//! cosmic-text attribute run. A table's cells are spans too, each marked with
//! where it sits ([`Cell`]), which the desktop lays out as a grid. This is
//! intentionally simple: no images or block backgrounds yet.

use crate::syntax::{self, Syntax};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// What Markdown this module reads, beyond CommonMark: task lists and tables,
/// both of which Obsidian renders. `is_simple` parses with the same set, or a
/// table would read to it as a paragraph of pipes.
const EXTENSIONS: Options = Options::ENABLE_TASKLISTS.union(Options::ENABLE_TABLES);

/// A run of text sharing one style.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    /// Monospace (inline code or code block).
    pub code: bool,
    /// 0 = body text; 1–6 = heading level.
    pub heading: u8,
    /// Part of a link (rendered in the accent color).
    pub link: bool,
    /// The link's destination, when this span is part of a link — from
    /// `[text](url)`, an `<url>` autolink, or a bare `http(s)://…` URL.
    pub url: Option<String>,
    /// Syntax class within a highlighted code block (`Plain` elsewhere).
    pub syntax: Syntax,
    /// Inside a blockquote — drawn muted, beside a bar.
    pub quote: bool,
    /// Set back from the body text: a finished task, a quote.
    pub muted: bool,
    /// The table cell this span is in, if it's in one.
    pub cell: Option<Cell>,
}

/// Where a span sits in a table. Cells come in reading order — the header row,
/// then each row left to right — and an empty cell has no spans, so a
/// renderer sizes the grid from the largest row and column it sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// Which table on the card, counting from 0, so two tables one after the
    /// other stay two grids.
    pub table: u16,
    pub row: u16,
    pub column: u16,
    /// In the header row: set bold, as Obsidian does.
    pub header: bool,
    /// The column's alignment, from its delimiter (`:---`, `:---:`, `---:`).
    pub align: CellAlign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellAlign {
    Left,
    Center,
    Right,
}

/// The table being read: which one, its columns' alignments, and the cell the
/// next span goes in.
struct Table {
    id: u16,
    aligns: Vec<Alignment>,
    row: u16,
    column: u16,
    header: bool,
}

impl Table {
    fn cell(&self) -> Cell {
        let align = match self.aligns.get(usize::from(self.column)) {
            Some(Alignment::Center) => CellAlign::Center,
            Some(Alignment::Right) => CellAlign::Right,
            _ => CellAlign::Left,
        };
        Cell { table: self.id, row: self.row, column: self.column, header: self.header, align }
    }
}

/// Parses `source` into styled spans. Block breaks are encoded as spans of
/// `"\n\n"`; list items get a `"• "` prefix and a trailing newline — a task
/// item a `"□ "` or `"☑ "` in its place, as Obsidian draws a checkbox rather
/// than a bullet. A finished task's text, and a quote's, are `muted`.
pub fn parse(source: &str) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut bold = 0u32;
    let mut italic = 0u32;
    let mut link = 0u32;
    // The destination of the link currently open (if any), so its text spans
    // carry it. Kept in lockstep with `link`.
    let mut link_url: Option<String> = None;
    let mut heading = 0u8;
    let mut list_depth = 0usize;
    let mut quote_depth = 0usize;
    // Inside a ticked task item, whose text is set back like a quote's.
    let mut task_done = false;
    // Fenced code is buffered across text events, then tokenized as a whole so
    // multi-line strings and block comments highlight correctly.
    let mut code_block: Option<CodeLang> = None;
    let mut table: Option<Table> = None;
    let mut tables = 0u16;

    let plain =
        |text: &str, code: bool, bold: bool, italic: bool, heading: u8, url: Option<String>| Span {
            text: text.to_owned(),
            bold,
            italic,
            code,
            heading,
            link: url.is_some(),
            url,
            syntax: Syntax::Plain,
            quote: false,
            muted: false,
            cell: None,
        };
    // Quote, done-task and table-cell styling is a property of where a span
    // falls, so it is stamped on here rather than threaded through every
    // constructor. A macro, not a closure: a closure would hold `out` for the
    // whole loop, and a task marker has to reach back into it.
    fn add(out: &mut Vec<Span>, span: Span, quote: bool, muted: bool, cell: Option<Cell>) {
        if !span.text.is_empty() {
            let bold = span.bold || cell.is_some_and(|cell| cell.header);
            out.push(Span { quote, muted: span.muted || muted, cell, bold, ..span });
        }
    }

    for event in Parser::new_ext(source, EXTENSIONS) {
        let (quoted, set_back) = (quote_depth > 0, quote_depth > 0 || task_done);
        let cell = table.as_ref().map(Table::cell);
        macro_rules! push {
            ($span:expr) => {
                add(&mut out, $span, quoted, set_back, cell)
            };
        }
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => heading = heading_level(level),
                Tag::Strong => bold += 1,
                Tag::Emphasis => italic += 1,
                Tag::Link { dest_url, .. } => {
                    link += 1;
                    link_url = Some(dest_url.to_string());
                }
                Tag::CodeBlock(kind) => code_block = Some(CodeLang::new(&kind)),
                Tag::List(_) => list_depth += 1,
                Tag::BlockQuote(_) => quote_depth += 1,
                Tag::Item => {
                    let indent = "    ".repeat(list_depth.saturating_sub(1));
                    push!(plain(&format!("{indent}\u{2022} "), false, false, false, 0, None));
                }
                Tag::Table(aligns) => {
                    table = Some(Table { id: tables, aligns, row: 0, column: 0, header: false });
                    tables += 1;
                }
                Tag::TableHead => {
                    if let Some(table) = &mut table {
                        table.header = true;
                    }
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(_) => {
                    heading = 0;
                    push!(plain("\n\n", false, false, false, 0, None));
                }
                TagEnd::Strong => bold = bold.saturating_sub(1),
                TagEnd::Emphasis => italic = italic.saturating_sub(1),
                TagEnd::Link => {
                    link = link.saturating_sub(1);
                    if link == 0 {
                        link_url = None;
                    }
                }
                TagEnd::CodeBlock => {
                    if let Some(block) = code_block.take() {
                        for (text, syntax) in block.highlight() {
                            push!(Span {
                                text,
                                bold: false,
                                italic: false,
                                code: true,
                                heading: 0,
                                link: false,
                                url: None,
                                syntax,
                                quote: false,
                                muted: false,
                                cell: None,
                            });
                        }
                    }
                    push!(plain("\n\n", false, false, false, 0, None));
                }
                TagEnd::Item => {
                    task_done = false;
                    push!(plain("\n", false, false, false, 0, None));
                }
                TagEnd::BlockQuote(_) => quote_depth = quote_depth.saturating_sub(1),
                TagEnd::List(_) => {
                    list_depth = list_depth.saturating_sub(1);
                    push!(plain("\n", false, false, false, 0, None));
                }
                TagEnd::Paragraph => push!(plain("\n\n", false, false, false, 0, None)),
                TagEnd::TableCell => {
                    if let Some(table) = &mut table {
                        table.column += 1;
                    }
                }
                // The header holds its cells directly, without a row of its own.
                TagEnd::TableHead | TagEnd::TableRow => {
                    if let Some(table) = &mut table {
                        (table.row, table.column, table.header) = (table.row + 1, 0, false);
                    }
                }
                TagEnd::Table => table = None,
                _ => {}
            },
            Event::Text(text) => match &mut code_block {
                Some(block) => block.source.push_str(&text),
                None => push!(plain(&text, false, bold > 0, italic > 0, heading, link_url.clone())),
            },
            Event::Code(text) => push!(plain(&text, true, bold > 0, italic > 0, heading, None)),
            // Obsidian's default renders a single newline as a line break
            // (not a re-flowed space), so preserve it.
            Event::SoftBreak => {
                push!(plain("\n", false, bold > 0, italic > 0, heading, link_url.clone()))
            }
            Event::HardBreak => push!(plain("\n", false, false, false, heading, None)),
            Event::Rule => {
                push!(plain("\u{2014}\u{2014}\u{2014}\n\n", false, false, false, 0, None))
            }
            // Arrives just after its item opened, so the bullet that item
            // pushed is the last span: a box goes in its place.
            Event::TaskListMarker(checked) => {
                if let Some(bullet) = out.last_mut().filter(|s| s.text.ends_with("\u{2022} ")) {
                    let indent = bullet.text.trim_end_matches("\u{2022} ").to_owned();
                    // □ and ☑ rather than the ☐ that pairs with ☑ by name:
                    // the system UI face has □ and ☑ at one size, and no ☐ —
                    // which fell back to Menlo, a size smaller and lighter.
                    bullet.text = format!("{indent}{} ", if checked { '\u{2611}' } else { '\u{25A1}' });
                    bullet.muted = checked;
                }
                task_done = checked;
            }
            _ => {}
        }
    }

    autolink_bare_urls(&mut out);
    trim_blank_edges(&mut out);
    out
}

/// Splits any bare `http(s)://…` URL out of plain spans into its own link span,
/// so plainly-typed URLs render styled and become clickable. Code spans and
/// spans already inside a Markdown/autolink link are left untouched.
fn autolink_bare_urls(spans: &mut Vec<Span>) {
    if !spans.iter().any(|s| !s.code && !s.link && has_bare_url(&s.text)) {
        return;
    }
    let mut out = Vec::with_capacity(spans.len());
    for span in spans.drain(..) {
        if span.code || span.link {
            out.push(span);
        } else {
            split_urls(span, &mut out);
        }
    }
    *spans = out;
}

fn has_bare_url(text: &str) -> bool {
    text.contains("http://") || text.contains("https://")
}

/// Pushes `span`, split around each bare URL into plain / link / plain spans
/// that inherit its style.
fn split_urls(span: Span, out: &mut Vec<Span>) {
    if !has_bare_url(&span.text) {
        out.push(span);
        return;
    }
    // Own the text so `span` stays available as the style template.
    let text = span.text.clone();
    let (mut i, mut last) = (0, 0);
    while i < text.len() {
        let rest = &text[i..];
        if rest.starts_with("http://") || rest.starts_with("https://") {
            let end = url_end(&text, i);
            if end > i {
                if last < i {
                    out.push(styled(&span, &text[last..i], None));
                }
                let url = &text[i..end];
                out.push(styled(&span, url, Some(url.to_owned())));
                (last, i) = (end, end);
                continue;
            }
        }
        i += rest.chars().next().map_or(1, char::len_utf8);
    }
    if last < text.len() {
        out.push(styled(&span, &text[last..], None));
    }
}

/// `span`'s style applied to different `text` and link destination.
fn styled(span: &Span, text: &str, url: Option<String>) -> Span {
    Span {
        text: text.to_owned(),
        link: url.is_some(),
        url,
        ..span.clone()
    }
}

/// The byte offset just past the bare URL starting at `start`: consumes
/// non-space, non-delimiter characters, then trims trailing sentence
/// punctuation and an unbalanced closing paren (so `(see https://x.com).` and
/// `https://x.com,` link only the URL).
fn url_end(text: &str, start: usize) -> usize {
    let mut end = start;
    for (off, ch) in text[start..].char_indices() {
        if ch.is_whitespace() || matches!(ch, '<' | '>' | '`' | '"' | '\\' | '|') {
            break;
        }
        end = start + off + ch.len_utf8();
    }
    let mut slice = &text[start..end];
    while let Some(last) = slice.chars().next_back() {
        let trim = matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '\'')
            || (last == ')' && slice.matches(')').count() > slice.matches('(').count());
        if !trim {
            break;
        }
        slice = &slice[..slice.len() - last.len_utf8()];
    }
    start + slice.len()
}

/// Whether `source` is simple enough to center: plain paragraphs (any number)
/// with only bold/italic emphasis. Any richer structure — headings, lists,
/// code, links, blockquotes, rules, images — reads better left-aligned.
/// Checked against the raw events (not our span model) so block constructs the
/// spans flatten away are still caught.
pub fn is_simple(source: &str) -> bool {
    for event in Parser::new_ext(source.trim(), EXTENSIONS) {
        match event {
            Event::Start(Tag::Paragraph | Tag::Emphasis | Tag::Strong)
            | Event::End(TagEnd::Paragraph | TagEnd::Emphasis | TagEnd::Strong)
            | Event::Text(_)
            | Event::SoftBreak
            | Event::HardBreak => {}
            // Headings, lists, code, links, quotes, rules, images, HTML, …
            _ => return false,
        }
    }
    true
}

/// A fenced code block accumulating its source, with the fence's language tag.
struct CodeLang {
    lang: String,
    source: String,
}

impl CodeLang {
    fn new(kind: &CodeBlockKind) -> Self {
        let lang = match kind {
            CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or("").to_owned(),
            CodeBlockKind::Indented => String::new(),
        };
        CodeLang { lang, source: String::new() }
    }

    fn highlight(&self) -> Vec<(String, Syntax)> {
        syntax::highlight(&self.source, &self.lang)
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Drops leading/trailing whitespace-only spans so the block never starts or
/// ends with blank lines.
/// `spans` with each table written out as rows of text — cells separated by
/// `" | "`, a row to a line — for a renderer that draws spans as they come and
/// has no grid. Without it a table's cells would run together into one word.
pub fn flatten_tables(spans: Vec<Span>) -> Vec<Span> {
    let mut out = Vec::with_capacity(spans.len());
    let mut last: Option<Cell> = None;
    for span in spans {
        let gap = match (last, span.cell) {
            (Some(was), Some(is)) if was.table == is.table && was.row == is.row && was.column != is.column => " | ",
            (Some(was), Some(is)) if was.table == is.table && was.row != is.row => "\n",
            (Some(_), _) => "\n\n",
            _ => "",
        };
        if !gap.is_empty() {
            out.push(Span { text: gap.to_owned(), bold: false, link: false, url: None, cell: None, ..span.clone() });
        }
        last = span.cell;
        out.push(Span { cell: None, ..span });
    }
    out
}

fn trim_blank_edges(spans: &mut Vec<Span>) {
    while spans.first().is_some_and(|s| s.text.trim().is_empty()) {
        spans.remove(0);
    }
    while spans.last().is_some_and(|s| s.text.trim().is_empty()) {
        spans.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(spans: &[Span]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn a_tables_cells_say_where_they_sit() {
        let spans = parse("Plan:\n\n| Day | Where |\n|---|:---:|\n| Fri | **Alfama** |\n| Sat | Belém |\n\nAfter.");
        let cells: Vec<(&str, u16, u16, bool)> = spans
            .iter()
            .filter_map(|s| s.cell.map(|c| (s.text.as_str(), c.row, c.column, c.header)))
            .collect();
        assert_eq!(
            cells,
            [
                ("Day", 0, 0, true),
                ("Where", 0, 1, true),
                ("Fri", 1, 0, false),
                ("Alfama", 1, 1, false),
                ("Sat", 2, 0, false),
                ("Belém", 2, 1, false),
            ]
        );
        // The header is bold, the column's alignment comes from its delimiter,
        // and what's around the table isn't in it.
        assert!(spans.iter().filter(|s| s.cell.is_some_and(|c| c.header)).all(|s| s.bold));
        assert!(spans.iter().filter(|s| s.cell.is_some_and(|c| c.column == 1)).all(|s| s.cell.unwrap().align == CellAlign::Center));
        assert_eq!(spans.first().unwrap().text, "Plan:");
        assert_eq!(spans.last().unwrap().text, "After.");
        assert!(spans.first().unwrap().cell.is_none() && spans.last().unwrap().cell.is_none());
        assert!(!is_simple("| a | b |\n|---|---|\n| 1 | 2 |"), "a table isn't centred like a paragraph");
    }

    #[test]
    fn a_table_flattens_to_rows_of_text() {
        let spans = flatten_tables(parse("| a | b |\n|---|---|\n| 1 | 2 |\n\nAfter."));
        assert_eq!(text_of(&spans), "a | b\n1 | 2\n\nAfter.");
        assert!(spans.iter().all(|s| s.cell.is_none()));
    }

    #[test]
    fn plain_text_is_a_single_body_span() {
        let spans = parse("just a label");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "just a label");
        assert!(!spans[0].bold && !spans[0].italic && spans[0].heading == 0);
    }

    #[test]
    fn inline_emphasis_and_code() {
        let spans = parse("a **bold** and *italic* and `code`");
        let bold = spans.iter().find(|s| s.text == "bold").unwrap();
        assert!(bold.bold && !bold.italic);
        let italic = spans.iter().find(|s| s.text == "italic").unwrap();
        assert!(italic.italic && !italic.bold);
        let code = spans.iter().find(|s| s.text == "code").unwrap();
        assert!(code.code);
    }

    #[test]
    fn heading_carries_its_level() {
        let spans = parse("# Title\n\nbody");
        let title = spans.iter().find(|s| s.text == "Title").unwrap();
        assert_eq!(title.heading, 1);
        let body = spans.iter().find(|s| s.text == "body").unwrap();
        assert_eq!(body.heading, 0);
    }

    #[test]
    fn list_items_get_bullets() {
        let spans = parse("- one\n- two");
        assert!(text_of(&spans).contains("\u{2022} one"));
        assert!(text_of(&spans).contains("\u{2022} two"));
    }

    #[test]
    fn task_items_get_a_box_in_place_of_the_bullet() {
        let spans = parse("- [ ] to do\n- [x] done\n- plain");
        let text = text_of(&spans);
        assert!(text.contains("\u{25A1} to do"), "{text:?}");
        assert!(text.contains("\u{2611} done"), "{text:?}");
        assert!(text.contains("\u{2022} plain"), "an ordinary item keeps its bullet");
        assert!(!text.contains("[ ]") && !text.contains("[x]"));

        let done = spans.iter().find(|s| s.text == "done").unwrap();
        let open = spans.iter().find(|s| s.text == "to do").unwrap();
        assert!(done.muted && !open.muted, "a finished task is set back, an open one isn't");
    }

    #[test]
    fn a_quote_is_marked_and_set_back() {
        let spans = parse("before\n\n> said *quietly*\n\nafter");
        let said = spans.iter().find(|s| s.text.starts_with("said")).unwrap();
        let quietly = spans.iter().find(|s| s.text == "quietly").unwrap();
        let after = spans.iter().find(|s| s.text == "after").unwrap();
        assert!(said.quote && said.muted && quietly.quote && quietly.italic);
        assert!(!after.quote && !after.muted);
    }

    #[test]
    fn links_are_flagged() {
        let spans = parse("see [the docs](https://x)");
        let link = spans.iter().find(|s| s.text == "the docs").unwrap();
        assert!(link.link);
        assert_eq!(link.url.as_deref(), Some("https://x"));
    }

    #[test]
    fn bare_urls_become_links() {
        let spans = parse("see https://example.com now");
        let link = spans.iter().find(|s| s.link).unwrap();
        assert_eq!(link.text, "https://example.com");
        assert_eq!(link.url.as_deref(), Some("https://example.com"));
        assert!(text_of(&spans).contains("see "));
        assert!(text_of(&spans).contains(" now"));
    }

    #[test]
    fn trailing_sentence_punctuation_stays_out_of_the_url() {
        let spans = parse("visit https://example.com.");
        let link = spans.iter().find(|s| s.link).unwrap();
        assert_eq!(link.url.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn urls_inside_code_are_not_linked() {
        let spans = parse("`https://example.com`");
        assert!(spans.iter().all(|s| !s.link));
    }

    #[test]
    fn a_wrapping_paren_excludes_the_closing_paren() {
        let spans = parse("(see https://example.com/path)");
        let link = spans.iter().find(|s| s.link).unwrap();
        assert_eq!(link.url.as_deref(), Some("https://example.com/path"));
    }

    #[test]
    fn simple_content_is_plain_or_emphasized() {
        assert!(is_simple("just a label"));
        assert!(is_simple("a **bold** and *italic* line"));
        // Line breaks and blank lines are still simple (plain text).
        assert!(is_simple("first line\nsecond line"));
        assert!(is_simple("first paragraph\n\nsecond paragraph"));
    }

    #[test]
    fn rich_content_is_not_simple() {
        assert!(!is_simple("# Heading"));
        assert!(!is_simple("has `code`"));
        assert!(!is_simple("a [link](x)"));
        assert!(!is_simple("- a list\n- item"));
        assert!(!is_simple("> a blockquote"));
    }

    #[test]
    fn single_newline_is_kept_as_a_line_break() {
        // Obsidian-style: a single newline stays a break, not a re-flowed space.
        let spans = parse("first line\nsecond line");
        assert!(text_of(&spans).contains("first line\nsecond line"));
    }
}
