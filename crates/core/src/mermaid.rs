//! Mermaid flowcharts read into cards: what [`crate::chain::mermaid`] writes,
//! and what an AI tool writes when it's asked for a diagram — pasted, they're
//! a board rather than a card holding their source.
//!
//! **Read in full or not at all.** A line this doesn't understand makes the
//! whole paste text again ([`parse`] is `None`), rather than cards with that
//! line quietly gone. What it understands and has no place for — a link's
//! style, a node's shape, a tooltip, accessibility text — it reads and lets
//! go. Ids are the chart's own and stand only for this paste: the caller gives
//! the cards fresh ones, as it does any it pastes.

use std::collections::HashMap;

use crate::layout::{self, Direction};
use crate::model::{self, CanvasEdge, CanvasNode, EdgeEnd};

/// A flowchart read into cards, links and groups, before it's laid out.
#[derive(Debug, Clone)]
pub struct Flowchart {
    /// The way it flows: `TD`, `TB` and `BT` down, `LR` and `RL` across.
    pub direction: Direction,
    /// Text cards, link cards (a node with a `click` to its own URL) and
    /// groups (subgraphs), with no size or place yet.
    pub nodes: Vec<CanvasNode>,
    pub edges: Vec<CanvasEdge>,
    /// The subgraph each node is in, by index into `nodes`. Groups on a board
    /// are positional, so this is what [`arrange`] turns into places.
    ///
    /// Mermaid's rule: a node is in the subgraph whose body names it, wherever
    /// else it's named — `c1-->a2` at the top and `a2` in `subgraph one` puts
    /// it in `one`. Named in two, the first to close keeps it, which puts a
    /// node in nested subgraphs in the innermost.
    pub parents: Vec<Option<usize>>,
}

/// Reads `text` as a Mermaid flowchart — fenced as a ```` ```mermaid ````
/// block, or bare from its `flowchart`/`graph` line — or `None` when it isn't
/// one, or has a line this can't read.
pub fn parse(text: &str) -> Option<Flowchart> {
    let source = unfence(text)?;
    let mut lines = source.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("%%")).peekable();
    // A `---` block of front matter (a title, a config) comes first.
    if lines.peek() == Some(&"---") {
        lines.next();
        lines.find(|&line| line == "---")?;
    }
    let (direction, rest) = header(lines.next()?)?;
    let mut chart = Chart::default();
    for line in rest.into_iter().chain(lines) {
        for statement in split_statements(line) {
            chart.statement(statement.trim())?;
        }
    }
    chart.finish(direction)
}

/// Places a flowchart's cards and sizes its groups, the inside of each group
/// first: its cards and the groups in it are laid out as a layered graph, and
/// its frame drawn around them, with `pad` at the sides and `head` above, where
/// the caption goes. Then the level above, with each group as one node — so a
/// subgraph comes out as a group whose cards are inside it, which is all a
/// group on a board is. Cards must be sized first; links are anchored to the
/// sides that face along the flow.
pub fn arrange(chart: &mut Flowchart, rank_gap: f32, node_gap: f32, pad: f32, head: f32) {
    let n = chart.nodes.len();
    let mut children: HashMap<Option<usize>, Vec<usize>> = HashMap::new();
    for i in 0..n {
        children.entry(chart.parents[i]).or_default().push(i);
    }
    lay_out(chart, &children, None, rank_gap, node_gap, pad, head);

    let index: HashMap<&str, usize> = chart.nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
    let centre = |node: &CanvasNode| (node.x + node.width / 2.0, node.y + node.height / 2.0);
    let centres: Vec<(f32, f32)> = chart.nodes.iter().map(centre).collect();
    for edge in &mut chart.edges {
        let (Some(&from), Some(&to)) = (index.get(edge.from_node.as_str()), index.get(edge.to_node.as_str())) else {
            continue;
        };
        let (from_side, to_side) = layout::facing_sides(centres[from], centres[to], chart.direction);
        edge.from_side = from_side.to_owned();
        edge.to_side = to_side.to_owned();
    }
}

/// Lays out what's directly in `container` (the top level when `None`), each
/// group in it laid out first, and frames `container` around the result.
#[allow(clippy::too_many_arguments)] // one recursive walk over the same chart
fn lay_out(
    chart: &mut Flowchart,
    children: &HashMap<Option<usize>, Vec<usize>>,
    container: Option<usize>,
    rank_gap: f32,
    node_gap: f32,
    pad: f32,
    head: f32,
) {
    let items = children.get(&container).cloned().unwrap_or_default();
    for &item in &items {
        if chart.nodes[item].is_group() {
            lay_out(chart, children, Some(item), rank_gap, node_gap, pad, head);
        }
    }
    if items.is_empty() {
        if let Some(group) = container {
            let frame = &mut chart.nodes[group];
            (frame.width, frame.height) = (4.0 * pad, head + 2.0 * pad);
        }
        return;
    }

    // Each link is between the two items here that hold its ends.
    let index: HashMap<&str, usize> = chart.nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
    let item_of = |mut i: usize| loop {
        match chart.parents[i] {
            parent if parent == container => return items.iter().position(|&it| it == i),
            Some(parent) => i = parent,
            None => return None,
        }
    };
    let links: Vec<(usize, usize)> = chart
        .edges
        .iter()
        .filter_map(|e| Some((item_of(*index.get(e.from_node.as_str())?)?, item_of(*index.get(e.to_node.as_str())?)?)))
        .filter(|(u, v)| u != v)
        .collect();
    let all: Vec<usize> = (0..items.len()).collect();
    let placed = {
        let sizes: Vec<CanvasNode> = items.iter().map(|&i| chart.nodes[i].clone()).collect();
        layout::place(&sizes, &all, &links, chart.direction, rank_gap, node_gap)
    };
    for (local, x, y) in placed {
        let item = items[local];
        let (dx, dy) = (x - chart.nodes[item].x, y - chart.nodes[item].y);
        for i in (0..chart.nodes.len()).filter(|&i| i == item || is_inside(&chart.parents, i, item)) {
            chart.nodes[i].x += dx;
            chart.nodes[i].y += dy;
        }
    }

    if let Some(group) = container {
        let (mut left, mut top, mut right, mut bottom) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &i in &items {
            let node = &chart.nodes[i];
            left = left.min(node.x);
            top = top.min(node.y);
            right = right.max(node.x + node.width);
            bottom = bottom.max(node.y + node.height);
        }
        let frame = &mut chart.nodes[group];
        (frame.x, frame.y) = (left - pad, top - head);
        (frame.width, frame.height) = (right - left + 2.0 * pad, bottom - top + head + pad);
    }
}

/// Whether node `i` is declared somewhere inside group `group`.
fn is_inside(parents: &[Option<usize>], mut i: usize, group: usize) -> bool {
    while let Some(parent) = parents[i] {
        if parent == group {
            return true;
        }
        i = parent;
    }
    false
}

/// The chart inside a ```` ```mermaid ```` fence, or `text` itself — and
/// nothing when there's more than the fence.
fn unfence(text: &str) -> Option<&str> {
    let text = text.trim();
    let Some(rest) = text.strip_prefix("```") else {
        return Some(text);
    };
    let (info, body) = rest.split_once('\n')?;
    (info.trim() == "mermaid").then_some(())?;
    body.trim_end().strip_suffix("```")
}

/// The direction a `flowchart` or `graph` line gives, and what follows it on
/// the line after a `;`.
fn header(line: &str) -> Option<(Direction, Option<&str>)> {
    let (first, rest) = match line.split_once(';') {
        Some((first, rest)) => (first, Some(rest.trim()).filter(|r| !r.is_empty())),
        None => (line, None),
    };
    let mut words = first.split_whitespace();
    let keyword = words.next()?;
    if !matches!(keyword, "flowchart" | "graph" | "flowchart-elk") {
        return None;
    }
    let direction = match words.next() {
        None | Some("TD" | "TB" | "BT") => Direction::Down,
        Some("LR" | "RL") => Direction::Right,
        Some(_) => return None,
    };
    words.next().is_none().then_some((direction, rest))
}

/// `line` split at each `;` outside quotes and brackets.
fn split_statements(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut depth, mut quoted) = (0, 0i32, false);
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '[' | '(' | '{' if !quoted => depth += 1,
            ']' | ')' | '}' if !quoted => depth -= 1,
            ';' if !quoted && depth <= 0 => {
                out.push(&line[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&line[start..]);
    out.into_iter().filter(|s| !s.trim().is_empty()).collect()
}

/// A flowchart as it's read: its nodes by id, the subgraphs open around the
/// line being read, and the styles to apply at the end.
#[derive(Default)]
struct Chart {
    nodes: Vec<CanvasNode>,
    parents: Vec<Option<usize>>,
    ids: HashMap<String, usize>,
    edges: Vec<CanvasEdge>,
    /// The subgraphs open around the line being read, innermost last, each
    /// with the nodes its body has named so far.
    open: Vec<(usize, Vec<usize>)>,
    /// Node id → fill, from `style` lines.
    styles: HashMap<String, String>,
    /// Class → fill, from `classDef`.
    classes: HashMap<String, String>,
    /// Node id → class, from `class` lines and `:::`.
    class_of: HashMap<String, String>,
    /// Node id → URL, from `click`.
    clicks: HashMap<String, String>,
}

/// A node as a statement names it: its id, and the text and class it may be
/// given there.
struct NodeRef {
    id: String,
    text: Option<String>,
    class: Option<String>,
}

/// A link as a statement draws it.
struct Link {
    from_arrow: bool,
    to_arrow: bool,
    label: Option<String>,
}

impl Chart {
    /// Reads one statement, or `None` if it can't.
    fn statement(&mut self, statement: &str) -> Option<()> {
        let (keyword, rest) = statement.split_once(char::is_whitespace).unwrap_or((statement, ""));
        let rest = rest.trim();
        match keyword {
            "end" if rest.is_empty() => {
                let (group, named) = self.open.pop()?;
                for i in named {
                    let open = self.open.iter().any(|&(g, _)| g == i);
                    if self.parents[i].is_none() && i != group && !open {
                        self.parents[i] = Some(group);
                    }
                }
            }
            "subgraph" => self.subgraph(rest),
            "direction" | "linkStyle" | "accTitle" | "accTitle:" | "accDescr" | "accDescr:" => {}
            "style" => {
                let (id, props) = rest.split_once(char::is_whitespace)?;
                if let Some(fill) = fill(props) {
                    self.styles.insert(id.to_owned(), fill);
                }
            }
            "classDef" => {
                let (names, props) = rest.split_once(char::is_whitespace)?;
                if let Some(fill) = fill(props) {
                    for name in names.split(',') {
                        self.classes.insert(name.trim().to_owned(), fill.clone());
                    }
                }
            }
            "class" => {
                let (ids, class) = rest.rsplit_once(char::is_whitespace)?;
                for id in ids.split(',') {
                    self.class_of.insert(id.trim().to_owned(), class.trim().to_owned());
                }
            }
            "click" => {
                let (id, action) = rest.split_once(char::is_whitespace)?;
                let action = action.trim();
                let action = action.strip_prefix("href").map(str::trim).unwrap_or(action);
                // A URL in quotes; a callback, `call` or otherwise, does nothing here.
                if let Some(url) = action.strip_prefix('"').and_then(|a| a.split_once('"')).map(|(url, _)| url) {
                    self.clicks.insert(id.to_owned(), url.replace("%22", "\""));
                }
            }
            _ => self.chain(statement)?,
        }
        Some(())
    }

    /// `subgraph id [title]`, `subgraph id["title"]`, `subgraph "title"` or
    /// `subgraph title words`: a group, open until its `end`.
    fn subgraph(&mut self, rest: &str) {
        let (id, title) = match rest.split_once('[') {
            Some((id, title)) => (id.trim().to_owned(), Some(label_text(title.trim_end().strip_suffix(']').unwrap_or(title)))),
            None if rest.starts_with('"') => (rest.to_owned(), Some(label_text(rest))),
            None if rest.is_empty() => (format!("subgraph {}", self.nodes.len()), None),
            None => (rest.to_owned(), Some(label_text(rest))),
        };
        let index = self.node(&id);
        let group = &mut self.nodes[index];
        group.kind = "group".to_owned();
        group.text = None;
        group.set_label(title.as_deref().unwrap_or(model::UNTITLED_GROUP));
        self.open.push((index, Vec::new()));
    }

    /// A statement of nodes and the links between them: `A[Text]`,
    /// `A --> B`, `A -- label --> B -->|another| C`, `A & B --> C`.
    fn chain(&mut self, statement: &str) -> Option<()> {
        let mut cursor = Cursor { s: statement.chars().collect(), i: 0 };
        let mut groups = vec![cursor.node_group()?];
        let mut links = Vec::new();
        loop {
            cursor.skip_space();
            if cursor.done() {
                break;
            }
            links.push(cursor.link()?);
            cursor.skip_space();
            groups.push(cursor.node_group()?);
        }
        let indices: Vec<Vec<usize>> = groups
            .into_iter()
            .map(|group| group.into_iter().map(|node| self.define(node)).collect())
            .collect();
        for (n, link) in links.iter().enumerate() {
            for &from in &indices[n] {
                for &to in &indices[n + 1] {
                    let mut edge = CanvasEdge {
                        id: format!("e{}", self.edges.len()),
                        from_node: self.nodes[from].id.clone(),
                        from_side: "right".to_owned(),
                        to_node: self.nodes[to].id.clone(),
                        to_side: "left".to_owned(),
                        extra: Default::default(),
                    };
                    let end = |arrow: bool| if arrow { EdgeEnd::Arrow } else { EdgeEnd::None };
                    edge.set_ends(end(link.from_arrow), end(link.to_arrow));
                    if let Some(label) = &link.label {
                        edge.set_label(label);
                    }
                    self.edges.push(edge);
                }
            }
        }
        Some(())
    }

    /// The node `reference` names, made on first mention, and given the text
    /// it carries.
    fn define(&mut self, reference: NodeRef) -> usize {
        let index = self.node(&reference.id);
        if let Some(text) = reference.text {
            self.nodes[index].text = Some(text);
        }
        if let Some(class) = reference.class {
            self.class_of.insert(reference.id, class);
        }
        index
    }

    /// The node with Mermaid id `id`, made as a text card saying its id if
    /// it's new, and named in the body of the subgraph open around it.
    fn node(&mut self, id: &str) -> usize {
        let index = self.ids.get(id).copied().unwrap_or_else(|| self.new_node(id));
        if let Some((_, named)) = self.open.last_mut() {
            named.push(index);
        }
        index
    }

    fn new_node(&mut self, id: &str) -> usize {
        let index = self.nodes.len();
        self.nodes.push(CanvasNode {
            id: format!("m{index}"),
            kind: "text".to_owned(),
            text: Some(id.to_owned()),
            file: None,
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
            color: None,
            extra: Default::default(),
        });
        self.parents.push(None);
        self.ids.insert(id.to_owned(), index);
        index
    }

    /// The chart, its styles and clicks applied — or `None` with a subgraph
    /// left open or no node at all.
    fn finish(mut self, direction: Direction) -> Option<Flowchart> {
        if !self.open.is_empty() || self.nodes.is_empty() {
            return None;
        }
        for (id, &index) in &self.ids {
            let node = &mut self.nodes[index];
            let fill = self.styles.get(id).or_else(|| self.class_of.get(id).and_then(|c| self.classes.get(c)));
            node.color = fill.cloned();
            if let Some(url) = self.clicks.get(id).filter(|_| !node.is_group()) {
                let text = node.text.take().unwrap_or_default();
                if text.trim().is_empty() || text.trim() == url.trim() {
                    // A web link card, as the copy writes one.
                    node.kind = "link".to_owned();
                    node.extra.insert("url".to_owned(), serde_json::Value::String(url.trim().to_owned()));
                } else {
                    node.text = Some(format!("[{}]({})", text.trim(), url.trim()));
                }
            }
        }
        Some(Flowchart { direction, nodes: self.nodes, edges: self.edges, parents: self.parents })
    }
}

/// The colour a `style` or `classDef` line fills with, as a card's colour: an
/// Obsidian preset where it's one exactly, else its hex. An alpha is dropped,
/// as the copy adds one to tint.
fn fill(props: &str) -> Option<String> {
    let value = props.split(',').find_map(|prop| prop.trim().strip_prefix("fill:"))?.trim();
    let value = value.trim_end_matches(";");
    let hex = value.strip_prefix('#');
    let value = match hex.map(str::len) {
        Some(8) => &value[..7],
        Some(4) => &value[..4],
        _ => value,
    };
    let colour = model::parse_css_color(value)?;
    let hex = model::color_to_hex(colour);
    let preset = (1..=6)
        .map(|n| n.to_string())
        .find(|code| model::parse_color(code).map(model::color_to_hex).as_deref() == Some(hex.as_str()));
    Some(preset.unwrap_or(hex))
}

/// A label as Mermaid writes it, as a card's text: unquoted, a Markdown
/// string's backticks dropped, `<br>` a line break, entity codes the
/// characters they stand for, and the copy's bullets a Markdown list again.
fn label_text(raw: &str) -> String {
    let mut text = raw.trim();
    if let Some(inner) = text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
        text = inner;
    }
    if let Some(inner) = text.strip_prefix('`').and_then(|t| t.strip_suffix('`')) {
        text = inner;
    }
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(['#', '&', '<']) {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let lower = rest.to_ascii_lowercase();
        if let Some(br) = ["<br>", "<br/>", "<br />"].iter().find(|br| lower.starts_with(*br)) {
            out.push('\n');
            rest = &rest[br.len()..];
            continue;
        }
        // Mermaid's `#quot;` and `#35;`, and HTML's `&quot;` and `&#35;`.
        let name = rest[1..].find(';').map(|k| &rest[1..1 + k]).filter(|_| !rest.starts_with('<'));
        let code = name.map(|n| if rest.starts_with('&') { n.strip_prefix('#').unwrap_or(n) } else { n });
        match code.filter(|c| c.len() <= 8).and_then(entity_char) {
            Some(c) => {
                out.push(c);
                rest = &rest[1 + name.map_or(0, str::len) + 1..];
            }
            None => {
                out.push_str(&rest[..1]);
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out.lines()
        .map(|line| {
            let line = line.trim_end();
            if let Some(item) = line.strip_prefix("• ") {
                format!("- {item}")
            } else if let Some(task) = line.strip_prefix("□ ") {
                format!("- [ ] {task}")
            } else if let Some(task) = line.strip_prefix("☑ ") {
                format!("- [x] {task}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

/// The character an entity code (`quot`, `35`) names.
fn entity_char(name: &str) -> Option<char> {
    match name {
        "quot" => Some('"'),
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        number if !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) => char::from_u32(number.parse().ok()?),
        _ => None,
    }
}

/// A statement being read, a character at a time.
struct Cursor {
    s: Vec<char>,
    i: usize,
}

impl Cursor {
    fn done(&self) -> bool {
        self.i >= self.s.len()
    }

    fn peek(&self, ahead: usize) -> Option<char> {
        self.s.get(self.i + ahead).copied()
    }

    fn at(&self, text: &str) -> bool {
        text.chars().enumerate().all(|(n, c)| self.peek(n) == Some(c))
    }

    fn skip_space(&mut self) {
        while self.peek(0).is_some_and(char::is_whitespace) {
            self.i += 1;
        }
    }

    /// Nodes joined by `&`.
    fn node_group(&mut self) -> Option<Vec<NodeRef>> {
        let mut group = vec![self.node_ref()?];
        loop {
            let mark = self.i;
            self.skip_space();
            if self.peek(0) == Some('&') {
                self.i += 1;
                self.skip_space();
                group.push(self.node_ref()?);
            } else {
                self.i = mark;
                return Some(group);
            }
        }
    }

    /// An id, then perhaps its shape with its text, a `@{ … }` with a label,
    /// and a `:::class`.
    fn node_ref(&mut self) -> Option<NodeRef> {
        let start = self.i;
        while self.peek(0).is_some_and(|c| c.is_alphanumeric() || c == '_') {
            self.i += 1;
        }
        if self.i == start {
            return None;
        }
        let id: String = self.s[start..self.i].iter().collect();
        let mut text = None;
        if matches!(self.peek(0), Some('[' | '(' | '{' | '>')) {
            text = Some(self.shape()?);
        } else if self.at("@{") {
            text = self.shape_data()?;
        }
        let mut class = None;
        if self.at(":::") {
            self.i += 3;
            let from = self.i;
            while self.peek(0).is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-') {
                self.i += 1;
            }
            class = Some(self.s[from..self.i].iter().collect());
        }
        Some(NodeRef { id, text, class })
    }

    /// A shape's text: `[…]`, `(…)`, `{…}`, `>…]` and their doubled and
    /// slanted forms, quoted or not.
    fn shape(&mut self) -> Option<String> {
        const SHAPES: [(&str, &[&str]); 12] = [
            ("(((", &[")))"]),
            ("([", &["])"]),
            ("[[", &["]]"]),
            ("[(", &[")]"]),
            ("((", &["))"]),
            ("{{", &["}}"]),
            ("[/", &["/]", "\\]"]),
            ("[\\", &["\\]", "/]"]),
            ("[", &["]"]),
            ("(", &[")"]),
            ("{", &["}"]),
            (">", &["]"]),
        ];
        let (open, closes) = SHAPES.iter().find(|(open, _)| self.at(open))?;
        self.i += open.chars().count();
        let from = self.i;
        if self.peek(0) == Some('"') {
            self.i += 1;
            while self.peek(0)? != '"' {
                self.i += 1;
            }
            self.i += 1;
            let quoted: String = self.s[from..self.i].iter().collect();
            self.skip_space();
            let close = closes.iter().find(|close| self.at(close))?;
            self.i += close.chars().count();
            return Some(label_text(&quoted));
        }
        loop {
            if self.done() {
                return None;
            }
            if let Some(close) = closes.iter().find(|close| self.at(close)) {
                let text: String = self.s[from..self.i].iter().collect();
                self.i += close.chars().count();
                return Some(label_text(&text));
            }
            self.i += 1;
        }
    }

    /// `@{ shape: …, label: "…" }`, Mermaid's newer way to give a node its
    /// shape: its label, if it has one.
    fn shape_data(&mut self) -> Option<Option<String>> {
        self.i += 2;
        let from = self.i;
        let mut quoted = false;
        loop {
            match self.peek(0)? {
                '"' => quoted = !quoted,
                '}' if !quoted => break,
                _ => {}
            }
            self.i += 1;
        }
        let data: String = self.s[from..self.i].iter().collect();
        self.i += 1;
        let label = data.split_once("label:").map(|(_, rest)| {
            let rest = rest.trim();
            match rest.strip_prefix('"').and_then(|r| r.split_once('"')) {
                Some((label, _)) => label_text(label),
                None => label_text(rest.split(',').next().unwrap_or(rest)),
            }
        });
        Some(label)
    }

    /// A link: its line (`-->`, `---`, `-.->`, `==>`, `<-->`, `--o`, longer
    /// ones too), with a label written into it (`-- yes -->`) or after it
    /// (`-->|yes|`).
    fn link(&mut self) -> Option<Link> {
        // An end that points: an arrowhead, or a circle or a cross (`o--o`,
        // `--x`), which a card's link has no shape for, read as the end the
        // link is drawn to.
        let from_arrow = self.peek(0) == Some('<')
            || (matches!(self.peek(0), Some('o' | 'x')) && matches!(self.peek(1), Some('-' | '=')));
        if from_arrow {
            self.i += 1;
        }
        let run = self.run();
        let mut label = None;
        // `-- text -->`, `== text ==>`, `-. text .->`: the line opened, its
        // label, and the line again to close it.
        if matches!(run.as_str(), "--" | "==" | "-.") && self.peek(0).is_some_and(char::is_whitespace) {
            let rest: String = self.s[self.i..].iter().collect();
            let closes = ["-->", "---", "==>", "===", ".->", ".-", "--o", "--x", "==o", "==x"];
            let at = closes.iter().filter_map(|close| rest.find(close)).min()?;
            label = Some(label_text(&rest[..at]));
            self.i += rest[..at].chars().count();
            self.run();
        } else if run.chars().count() < 2 {
            return None;
        }
        let to_arrow = self.peek(0) == Some('>')
            || (matches!(self.peek(0), Some('o' | 'x')) && !self.peek(1).is_some_and(|c| c.is_alphanumeric() || c == '_'));
        if to_arrow {
            self.i += 1;
        } else if label.is_none() && run.chars().count() < 3 {
            return None;
        }
        self.skip_space();
        if self.peek(0) == Some('|') {
            self.i += 1;
            let from = self.i;
            while self.peek(0)? != '|' {
                self.i += 1;
            }
            label = Some(label_text(&self.s[from..self.i].iter().collect::<String>()));
            self.i += 1;
        }
        Some(Link { from_arrow, to_arrow, label: label.filter(|l| !l.is_empty()) })
    }

    /// The `-`, `=` and `.` a link's line is drawn with.
    fn run(&mut self) -> String {
        let from = self.i;
        while matches!(self.peek(0), Some('-' | '=' | '.')) {
            self.i += 1;
        }
        self.s[from..self.i].iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain;
    use crate::fixtures::{edge, node};

    /// The node whose text is `text`.
    fn find<'a>(chart: &'a Flowchart, text: &str) -> &'a CanvasNode {
        chart.nodes.iter().find(|n| n.text.as_deref() == Some(text)).unwrap_or_else(|| panic!("no {text:?} in {chart:#?}"))
    }

    fn edge_between<'a>(chart: &'a Flowchart, from: &str, to: &str) -> &'a CanvasEdge {
        let (from, to) = (&find(chart, from).id, &find(chart, to).id);
        chart.edges.iter().find(|e| &e.from_node == from && &e.to_node == to).expect("a link")
    }

    #[test]
    fn what_the_copy_writes_reads_back_as_the_same_board() {
        let mut nodes = vec![
            CanvasNode { text: Some("**Login** fails".to_owned()), ..node("symptom") },
            CanvasNode { kind: "group".to_owned(), text: None, x: -50.0, y: -50.0, width: 600.0, height: 400.0, ..node("tried") },
            CanvasNode { text: Some("Clock skew\n\n- NTP \"synced\"".to_owned()), ..node("skew") },
            CanvasNode { text: Some("C# & 1 > 0".to_owned()), x: 900.0, ..node("next") },
            CanvasNode { kind: "link".to_owned(), text: None, x: 900.0, y: 300.0, ..node("docs") },
        ];
        nodes[1].set_label("Tried");
        nodes[3].color = Some("1".to_owned());
        nodes[4].extra.insert("url".to_owned(), serde_json::json!("https://example.com/a\"b"));
        let mut ruled_out = edge("symptom", "skew");
        ruled_out.set_label("ruled out");
        let mut both = edge("skew", "next");
        both.set_ends(EdgeEnd::Arrow, EdgeEnd::Arrow);
        let mut plain = edge("next", "docs");
        plain.set_ends(EdgeEnd::None, EdgeEnd::None);
        let edges = vec![ruled_out, both, plain];

        let written = chain::mermaid(&nodes, &edges, &[0, 1, 2, 3, 4], Direction::Right);
        let chart = parse(&written).expect("its own flowchart");
        assert_eq!(chart.direction, Direction::Right);
        // Text as the board draws it — the flowchart has no bold — with the
        // list a list again.
        assert_eq!(find(&chart, "Clock skew\n- NTP \"synced\"").kind, "text");
        find(&chart, "Login fails");
        assert_eq!(find(&chart, "C# & 1 > 0").color.as_deref(), Some("1"), "a preset stays a preset");
        let group = chart.nodes.iter().position(|n| n.is_group()).expect("the group");
        assert_eq!(chart.nodes[group].label(), Some("Tried"));
        let skew = chart.nodes.iter().position(|n| n.text.as_deref() == Some("Clock skew\n- NTP \"synced\"")).unwrap();
        assert_eq!(chart.parents[skew], Some(group));
        let link = chart.nodes.iter().find(|n| n.kind == "link").expect("a link card");
        assert_eq!(link.url(), Some("https://example.com/a\"b"));

        assert_eq!(edge_between(&chart, "Login fails", "Clock skew\n- NTP \"synced\"").label(), Some("ruled out"));
        let both = edge_between(&chart, "Clock skew\n- NTP \"synced\"", "C# & 1 > 0");
        assert_eq!((both.from_end(), both.to_end()), (EdgeEnd::Arrow, EdgeEnd::Arrow));
        let plain = chart.edges.iter().find(|e| e.to_node == link.id).expect("to the link card");
        assert_eq!((plain.from_end(), plain.to_end()), (EdgeEnd::None, EdgeEnd::None));
    }

    #[test]
    fn a_chart_as_an_ai_writes_one() {
        let chart = parse(
            "```mermaid\n\
             flowchart TD\n    \
                 %% the happy path first\n    \
                 A[Start] --> B{Is it working?}\n    \
                 B -- Yes --> C[\"Great!\"]\n    \
                 B -->|No| D(Debug it)\n    \
                 D -.-> B\n    \
                 C & D ==> E((Done))\n    \
                 classDef warn fill:#f96,stroke:#333\n    \
                 class D warn\n    \
                 click C href \"https://example.com/docs\" \"Docs\"\n\
             ```",
        )
        .expect("a flowchart");
        assert_eq!(chart.direction, Direction::Down);
        assert_eq!(chart.nodes.len(), 5);
        assert_eq!(edge_between(&chart, "Is it working?", "[Great!](https://example.com/docs)").label(), Some("Yes"));
        assert_eq!(edge_between(&chart, "Is it working?", "Debug it").label(), Some("No"));
        edge_between(&chart, "Debug it", "Is it working?");
        edge_between(&chart, "Debug it", "Done");
        edge_between(&chart, "[Great!](https://example.com/docs)", "Done");
        assert_eq!(chart.edges.len(), 6);
        assert_eq!(find(&chart, "Debug it").color.as_deref(), Some("#ff9966"));
    }

    #[test]
    fn subgraphs_semicolons_entities_and_front_matter() {
        let chart = parse(
            "---\ntitle: Trip\n---\n\
             graph LR; a[\"Pack #quot;light#quot;<br>one bag\"] --> b\n\
             subgraph days [Day one]\n  b --- c[Fado &amp; dinner]\n  subgraph inner\n    d\n  end\nend\n\
             style c fill:#44cf6e",
        )
        .expect("a flowchart");
        assert_eq!(chart.direction, Direction::Right);
        find(&chart, "Pack \"light\"\none bag");
        let days = chart.nodes.iter().position(|n| n.label() == Some("Day one")).expect("the subgraph");
        let inner = chart.nodes.iter().position(|n| n.label() == Some("inner")).expect("the nested one");
        assert_eq!(chart.parents[inner], Some(days));
        // Named first outside, then in the subgraph: it's in the subgraph.
        let at = |text: &str| chart.nodes.iter().position(|n| n.text.as_deref() == Some(text)).unwrap();
        assert_eq!(chart.parents[at("b")], Some(days));
        assert_eq!(chart.parents[at("Fado & dinner")], Some(days));
        assert_eq!(chart.parents[at("Pack \"light\"\none bag")], None);
        assert_eq!(chart.parents[at("d")], Some(inner));
        assert_eq!(find(&chart, "Fado & dinner").color.as_deref(), Some("4"));
    }

    #[test]
    fn anything_it_cant_read_stays_text() {
        for text in [
            "just a sentence about flowcharts",
            "sequenceDiagram\n    A->>B: hi",
            "flowchart LR\n    A -- B",
            "flowchart LR\n    A --> B\n    pie title Pets",
            "flowchart LR\n    subgraph open\n    A --> B",
            "Here's the chart:\n```mermaid\nflowchart LR\nA --> B\n```",
            "flowchart LR",
        ] {
            assert!(parse(text).is_none(), "{text:?}");
        }
    }

    #[test]
    fn a_group_is_arranged_around_its_cards() {
        let mut chart = parse("flowchart LR\n  start --> a\n  subgraph g [Group]\n    a --> b\n  end\n  b --> finish").unwrap();
        for node in chart.nodes.iter_mut().filter(|n| !n.is_group()) {
            (node.width, node.height) = (160.0, 60.0);
        }
        arrange(&mut chart, 70.0, 40.0, 24.0, 44.0);
        let group = chart.nodes.iter().find(|n| n.is_group()).unwrap().clone();
        let (a, b) = (find(&chart, "a"), find(&chart, "b"));
        for card in [a, b] {
            assert!(model::centre_within(card, &group), "{} inside", card.text.as_deref().unwrap());
            assert!(card.x - group.x >= 24.0 - 0.01 && card.y - group.y >= 44.0 - 0.01, "with room for the caption");
        }
        assert!(a.x < b.x, "across, inside it too");
        let (start, finish) = (find(&chart, "start"), find(&chart, "finish"));
        assert!(start.x + start.width < group.x && group.x + group.width < finish.x, "the group ranked between");
        assert!(!model::centre_within(start, &group) && !model::centre_within(finish, &group));
        let into = edge_between(&chart, "start", "a");
        assert_eq!((into.from_side.as_str(), into.to_side.as_str()), ("right", "left"));
    }
}
