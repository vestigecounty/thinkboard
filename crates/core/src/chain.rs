//! Cards copied out of the board as text another app reads: the selection, or
//! the chain of cards leading into it, written as Markdown — what a copy puts
//! in the clipboard's plain text — or as a Mermaid flowchart.
//!
//! **Written for a person or an agent to read.** One card is its own text.
//! Two or more are a list, a card to an item, its own lines indented under it:
//! joined by blank lines, they ran together wherever a card had a paragraph
//! break of its own. A link's label leads the card it points to ("→ ruled
//! out: …"), so the reasoning along a chain survives being copied out of it.
//! A web link card is its URL and a file card its path in the vault.
//!
//! Ask AI reads the same cards ([`upstream`] is its walk) but keeps its own
//! prompt, blank-line-joined text ([`crate::ai::upstream_prompt`]): its answer
//! is split into cards at `---`, and a prompt of another shape can change the
//! shape of the answer. This one hasn't been tried as a prompt.

use std::collections::{HashMap, HashSet};

use crate::layout::Direction;
use crate::markdown;
use crate::model::{self, CanvasEdge, CanvasNode, EdgeEnd, NodeKind};

/// What one card says: its text, a web link card's URL, a file card's path in
/// the vault. A group says nothing of its own — what it encloses is its
/// outline ([`crate::ai::group_prompt`]).
pub fn card_text(node: &CanvasNode) -> Option<String> {
    let text = match node.node_kind() {
        NodeKind::Group => return None,
        NodeKind::Link => node.url()?,
        NodeKind::Text => node.text.as_deref()?,
        NodeKind::Image | NodeKind::Note | NodeKind::Attachment => node.file.as_deref()?,
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// `set` and everything a group in it encloses, each once: what a copy
/// carries. A group is copied with its cards, as it's dragged with them.
pub fn with_contents(nodes: &[CanvasNode], set: &[usize]) -> Vec<usize> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for &i in set {
        let contents = if nodes[i].is_group() { model::enclosed_by(nodes, i) } else { Vec::new() };
        for j in std::iter::once(i).chain(contents) {
            if seen.insert(j) {
                out.push(j);
            }
        }
    }
    out
}

/// Each of `targets` and every card leading into it — what Ask AI reads for
/// each ([`crate::ai::upstream_nodes`]) — each once. A group is itself alone:
/// it's a region rather than a step in a chain, and what it encloses is what's
/// copied of it.
pub fn upstream(nodes: &[CanvasNode], edges: &[CanvasEdge], targets: &[usize]) -> Vec<usize> {
    let index: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for &target in targets {
        let chain: Vec<usize> = if nodes[target].is_group() {
            vec![target]
        } else {
            crate::ai::upstream_nodes(nodes, edges, &nodes[target].id)
                .iter()
                .filter_map(|node| index.get(node.id.as_str()).copied())
                .collect()
        };
        for i in chain {
            if seen.insert(i) {
                out.push(i);
            }
        }
    }
    out
}

/// The entries `set` is written as, in its own order: each card and group in
/// it, less what a group in it encloses, which that group's outline brings.
pub fn entries(nodes: &[CanvasNode], set: &[usize]) -> Vec<usize> {
    let mut seen = HashSet::new();
    set.iter()
        .copied()
        .filter(|&i| enclosing_group(nodes, set, i).is_none() && seen.insert(i))
        .collect()
}

/// [`entries`] in link order: a card before the ones it links to — a link to
/// or from what a group encloses counting as the group's — with ties, and
/// cycles, in reading order: top to bottom, then left to right.
pub fn order(nodes: &[CanvasNode], edges: &[CanvasEdge], set: &[usize]) -> Vec<usize> {
    let entries = entries(nodes, set);
    let links: Vec<(usize, usize)> = entry_links(nodes, edges, &entries).into_iter().map(|(u, v, _)| (u, v)).collect();
    topo(nodes, &entries, &links)
}

/// `entries` ([`order`], [`entries`]) in the chain format: one alone is its
/// own text, and two or more a Markdown list, a line of each item's own
/// indented under it. A group is its outline, its name leading it. A labelled
/// link into an entry from another leads that entry: "→ ruled out: …".
pub fn text(nodes: &[CanvasNode], edges: &[CanvasEdge], entries: &[usize]) -> String {
    let links = entry_links(nodes, edges, entries);
    let several = entries.len() > 1;
    let mut items: Vec<String> = entries
        .iter()
        .filter_map(|&e| {
            let body = if nodes[e].is_group() {
                let outline = crate::ai::group_outline(nodes, edges, e, crate::ai::Outline::Copy);
                if outline.is_empty() {
                    return None;
                }
                // As one item among others, an unnamed group still needs a
                // line to lead its contents, or its first card would read as
                // the item.
                let named = nodes[e].label().is_some_and(|l| !l.trim().is_empty());
                if several && !named { format!("{}\n{outline}", model::UNTITLED_GROUP) } else { outline }
            } else {
                card_text(&nodes[e])?
            };
            let mut labels: Vec<&str> = Vec::new();
            for &(_, to, label) in &links {
                if let Some(label) = label.filter(|l| to == e && !labels.contains(l)) {
                    labels.push(label);
                }
            }
            Some(if labels.is_empty() { body } else { format!("→ {}: {body}", labels.join(", ")) })
        })
        .collect();
    if items.len() <= 1 {
        return items.pop().unwrap_or_default();
    }
    let mut out = String::new();
    for item in &items {
        list_item(&mut out, "", item);
    }
    out.trim_end().to_owned()
}

/// Writes `body` as a list item at `indent`: its first line after the bullet,
/// the rest indented under it so they stay in the item — as a group's outline
/// always has, Ask AI's included.
pub(crate) fn list_item(out: &mut String, indent: &str, body: &str) {
    for (n, line) in body.lines().enumerate() {
        if n == 0 {
            out.push_str(&format!("{indent}- {line}\n"));
        } else {
            out.push_str(&format!("{indent}  {line}\n"));
        }
    }
}

/// Markdown as HTML, for the apps that paste HTML over plain text: Obsidian
/// turns it back into Markdown, and a mail or a document keeps its formatting
/// rather than showing the markers.
pub fn html(markdown: &str) -> String {
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, pulldown_cmark::Parser::new_ext(markdown, markdown::EXTENSIONS));
    out
}

/// `set` as a Mermaid flowchart flowing in `dir`, fenced as a ```` ```mermaid ````
/// block, which Obsidian and GitHub draw where it's pasted. Every card is a
/// node, declared in link order, labelled with its whole text — as plain text,
/// since Mermaid shows Markdown's markers as written. A group is a subgraph
/// around what it encloses, nesting kept. Links keep their labels and their
/// ends, a web link card its URL to click, and a coloured card its colour.
pub fn mermaid(nodes: &[CanvasNode], edges: &[CanvasEdge], set: &[usize], dir: Direction) -> String {
    let cards: Vec<usize> = set.iter().copied().filter(|&i| !nodes[i].is_group()).collect();
    let mut groups: Vec<usize> = set.iter().copied().filter(|&i| nodes[i].is_group()).collect();
    groups.sort_by(|&a, &b| reading(nodes, a, b));
    let index: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
    let card_links: Vec<(usize, usize)> = edges
        .iter()
        .filter_map(|e| Some((*index.get(e.from_node.as_str())?, *index.get(e.to_node.as_str())?)))
        .filter(|&(u, v)| u != v && cards.contains(&u) && cards.contains(&v))
        .collect();
    let cards = topo(nodes, &cards, &card_links);

    let mut ids: HashMap<usize, String> = HashMap::new();
    for (n, &i) in cards.iter().enumerate() {
        ids.insert(i, format!("n{}", n + 1));
    }
    for (n, &g) in groups.iter().enumerate() {
        ids.insert(g, format!("g{}", n + 1));
    }
    // Where each is declared: in the innermost group of the set around it.
    let innermost = |i: usize| {
        groups
            .iter()
            .copied()
            .filter(|&g| g != i && model::centre_within(&nodes[i], &nodes[g]))
            .min_by(|&a, &b| area(nodes, a).total_cmp(&area(nodes, b)))
    };
    let within: HashMap<usize, Option<usize>> = cards.iter().chain(&groups).map(|&i| (i, innermost(i))).collect();

    let mut out = format!("```mermaid\nflowchart {}\n", if dir == Direction::Down { "TD" } else { "LR" });
    declare(nodes, &cards, &groups, &ids, &within, None, 1, &mut out);
    for edge in edges {
        let (Some(from), Some(to)) = (
            index.get(edge.from_node.as_str()).and_then(|i| ids.get(i)),
            index.get(edge.to_node.as_str()).and_then(|i| ids.get(i)),
        ) else {
            continue;
        };
        // Mermaid has no arrow at the source alone, so that link is written
        // the other way round, which draws the same.
        let (from, arrow, to) = match (edge.from_end(), edge.to_end()) {
            (EdgeEnd::None, EdgeEnd::Arrow) => (from, "-->", to),
            (EdgeEnd::Arrow, EdgeEnd::None) => (to, "-->", from),
            (EdgeEnd::Arrow, EdgeEnd::Arrow) => (from, "<-->", to),
            (EdgeEnd::None, EdgeEnd::None) => (from, "---", to),
        };
        match edge.label() {
            Some(label) => out.push_str(&format!("    {from} {arrow}|\"{}\"| {to}\n", mermaid_escape(label.trim()))),
            None => out.push_str(&format!("    {from} {arrow} {to}\n")),
        }
    }
    for &i in cards.iter().chain(&groups) {
        if nodes[i].node_kind() == NodeKind::Link
            && let Some(url) = nodes[i].url()
        {
            out.push_str(&format!("    click {} \"{}\"\n", ids[&i], url.trim().replace('"', "%22")));
        }
    }
    for &i in cards.iter().chain(&groups) {
        if let Some(colour) = nodes[i].palette_color() {
            let hex = model::color_to_hex(colour);
            out.push_str(&format!("    style {} fill:{hex}33,stroke:{hex}\n", ids[&i]));
        }
    }
    out.push_str("```");
    out
}

/// Declares the cards and groups directly in `container` (the top level when
/// `None`): the cards in link order, then each group as a subgraph of its own.
#[allow(clippy::too_many_arguments)] // one recursive walk over the same tables
fn declare(
    nodes: &[CanvasNode],
    cards: &[usize],
    groups: &[usize],
    ids: &HashMap<usize, String>,
    within: &HashMap<usize, Option<usize>>,
    container: Option<usize>,
    depth: usize,
    out: &mut String,
) {
    let pad = "    ".repeat(depth);
    for &i in cards.iter().filter(|i| within[i] == container) {
        out.push_str(&format!("{pad}{}[\"{}\"]\n", ids[&i], mermaid_card_label(&nodes[i])));
    }
    for &g in groups.iter().filter(|g| within[g] == container) {
        let title = nodes[g].label().map(str::trim).filter(|l| !l.is_empty()).unwrap_or(model::UNTITLED_GROUP);
        out.push_str(&format!("{pad}subgraph {}[\"{}\"]\n", ids[&g], mermaid_escape(title)));
        declare(nodes, cards, groups, ids, within, Some(g), depth + 1, out);
        out.push_str(&format!("{pad}end\n"));
    }
}

/// A card's text as a Mermaid node's label: its Markdown read as plain text
/// (a list item's bullet kept), a line break to each line, and nothing
/// Mermaid would read as its own.
fn mermaid_card_label(node: &CanvasNode) -> String {
    let Some(text) = card_text(node) else {
        return " ".to_owned();
    };
    let plain: String = if node.is_text() {
        markdown::flatten_tables(markdown::parse(&text)).into_iter().map(|span| span.text).collect()
    } else {
        text
    };
    plain
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .map(mermaid_escape)
        .collect::<Vec<_>>()
        .join("<br>")
}

/// `text` with what Mermaid reads as syntax written as its entity codes,
/// which it shows as the characters: a quote would end the label, `<` and
/// `>` would be markup, a backtick would start a Markdown string, and `#` and
/// `&` start entities of their own.
fn mermaid_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '"' => out.push_str("#quot;"),
            '<' => out.push_str("#lt;"),
            '>' => out.push_str("#gt;"),
            '`' => out.push_str("#96;"),
            '#' => out.push_str("#35;"),
            '&' => out.push_str("#amp;"),
            c => out.push(c),
        }
    }
    out
}

/// The links between `entries`, each end taken to the entry it's written
/// under — a link to or from what a group encloses is the group's — with
/// its label. Links within one entry drop out.
fn entry_links<'a>(nodes: &[CanvasNode], edges: &'a [CanvasEdge], entries: &[usize]) -> Vec<(usize, usize, Option<&'a str>)> {
    let index: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
    let entry_of = |i: usize| enclosing_group(nodes, entries, i).or_else(|| entries.contains(&i).then_some(i));
    edges
        .iter()
        .filter_map(|edge| {
            let from = entry_of(*index.get(edge.from_node.as_str())?)?;
            let to = entry_of(*index.get(edge.to_node.as_str())?)?;
            (from != to).then(|| (from, to, edge.label().map(str::trim)))
        })
        .collect()
}

/// The outermost group among `set` that encloses node `i`, if one does.
fn enclosing_group(nodes: &[CanvasNode], set: &[usize], i: usize) -> Option<usize> {
    set.iter()
        .copied()
        .filter(|&g| g != i && nodes[g].is_group() && model::centre_within(&nodes[i], &nodes[g]))
        .max_by(|&a, &b| area(nodes, a).total_cmp(&area(nodes, b)))
}

/// `items`, each before the ones `links` lead to from it; ties, and a cycle,
/// broken in reading order.
fn topo(nodes: &[CanvasNode], items: &[usize], links: &[(usize, usize)]) -> Vec<usize> {
    let mut indegree: HashMap<usize, usize> = items.iter().map(|&i| (i, 0)).collect();
    for &(u, v) in links {
        if indegree.contains_key(&u)
            && let Some(n) = indegree.get_mut(&v)
        {
            *n += 1;
        }
    }
    let mut left: Vec<usize> = items.to_vec();
    let mut out = Vec::with_capacity(items.len());
    while !left.is_empty() {
        // The first in reading order of what nothing left leads to — or, in a
        // cycle, of everything left.
        let ready = left.iter().copied().filter(|i| indegree[i] == 0).min_by(|&a, &b| reading(nodes, a, b));
        let next = ready
            .or_else(|| left.iter().copied().min_by(|&a, &b| reading(nodes, a, b)))
            .expect("something is left");
        left.retain(|&i| i != next);
        out.push(next);
        for &(u, v) in links {
            if u == next
                && let Some(n) = indegree.get_mut(&v)
            {
                *n = n.saturating_sub(1);
            }
        }
    }
    out
}

/// Reading order: top to bottom, then left to right.
fn reading(nodes: &[CanvasNode], a: usize, b: usize) -> std::cmp::Ordering {
    nodes[a].y.total_cmp(&nodes[b].y).then(nodes[a].x.total_cmp(&nodes[b].x))
}

fn area(nodes: &[CanvasNode], i: usize) -> f32 {
    nodes[i].width * nodes[i].height
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{edge, node};
    use serde_json::{Value, json};

    /// A text card at `(x, y)`.
    fn card(id: &str, text: &str, x: f32, y: f32) -> CanvasNode {
        CanvasNode { text: Some(text.to_owned()), x, y, ..node(id) }
    }

    fn group(id: &str, label: &str, x: f32, y: f32, width: f32, height: f32) -> CanvasNode {
        let mut group = CanvasNode { kind: "group".to_owned(), text: None, x, y, width, height, ..node(id) };
        group.set_label(label);
        group
    }

    fn labelled(from: &str, to: &str, label: &str) -> CanvasEdge {
        let mut edge = edge(from, to);
        edge.set_label(label);
        edge
    }

    /// A bug being reasoned about: a symptom, a cause ruled out, the next
    /// thing to try, laid out left to right.
    fn a_chain() -> (Vec<CanvasNode>, Vec<CanvasEdge>) {
        let nodes = vec![
            card("symptom", "Login fails after the token refresh", 0.0, 0.0),
            card("skew", "Clock skew\n\nBoth servers are NTP-synced.", 300.0, 0.0),
            card("cache", "The token cache is shared between tabs", 600.0, 0.0),
        ];
        let edges = vec![labelled("symptom", "skew", "ruled out"), edge("skew", "cache")];
        (nodes, edges)
    }

    #[test]
    fn a_card_says_its_text_its_url_or_its_path() {
        assert_eq!(card_text(&card("a", "  some text \n", 0.0, 0.0)).as_deref(), Some("some text"));
        let mut link = CanvasNode { kind: "link".to_owned(), text: None, ..node("l") };
        link.extra.insert("url".to_owned(), Value::String("https://example.com".to_owned()));
        assert_eq!(card_text(&link).as_deref(), Some("https://example.com"));
        let file = |path: &str| CanvasNode { kind: "file".to_owned(), text: None, file: Some(path.to_owned()), ..node("f") };
        assert_eq!(card_text(&file("Notes/Login bug.md")).as_deref(), Some("Notes/Login bug.md"));
        assert_eq!(card_text(&file("pics/trace.png")).as_deref(), Some("pics/trace.png"));
        assert_eq!(card_text(&group("g", "Packing", 0.0, 0.0, 100.0, 100.0)), None, "a group is its outline");
        assert_eq!(card_text(&card("e", "   ", 0.0, 0.0)), None);
    }

    #[test]
    fn one_card_is_its_own_text() {
        let (nodes, edges) = a_chain();
        assert_eq!(text(&nodes, &edges, &[1]), "Clock skew\n\nBoth servers are NTP-synced.");
    }

    #[test]
    fn a_chain_is_a_list_its_labels_leading_the_cards_they_point_to() {
        let (nodes, edges) = a_chain();
        let entries = order(&nodes, &edges, &[2, 0, 1]);
        assert_eq!(entries, vec![0, 1, 2], "in link order, whatever the selection's");
        assert_eq!(
            text(&nodes, &edges, &entries),
            "- Login fails after the token refresh\n\
             - → ruled out: Clock skew\n  \n  \
             Both servers are NTP-synced.\n\
             - The token cache is shared between tabs"
        );
    }

    #[test]
    fn unlinked_cards_go_in_reading_order() {
        let nodes = vec![card("right", "R", 400.0, 0.0), card("below", "B", 0.0, 300.0), card("left", "L", 0.0, 0.0)];
        assert_eq!(order(&nodes, &[], &[0, 1, 2]), vec![2, 0, 1]);
    }

    #[test]
    fn a_cycle_is_ordered_and_ends() {
        let nodes = vec![card("a", "A", 0.0, 0.0), card("b", "B", 300.0, 0.0)];
        let edges = vec![edge("a", "b"), edge("b", "a")];
        assert_eq!(order(&nodes, &edges, &[1, 0]), vec![0, 1]);
    }

    #[test]
    fn a_group_is_its_outline_and_carries_its_links() {
        // The group's card links on to one outside it.
        let nodes = vec![
            card("before", "Plan the trip", -600.0, 0.0),
            group("packing", "Packing", 0.0, 0.0, 400.0, 300.0),
            card("passport", "Passport", 50.0, 50.0),
            card("after", "Leave", 900.0, 0.0),
        ];
        let edges = vec![edge("before", "passport"), labelled("passport", "after", "then")];
        // Its card selected too: the outline already brings it.
        let entries = order(&nodes, &edges, &[3, 2, 1, 0]);
        assert_eq!(entries, vec![0, 1, 3]);
        assert_eq!(text(&nodes, &edges, &entries), "- Plan the trip\n- Packing\n  - Passport\n- → then: Leave");
        // An unnamed one among others still has a line to lead it.
        let mut unnamed = nodes.clone();
        unnamed[1].set_label("");
        assert_eq!(text(&unnamed, &edges, &entries), "- Plan the trip\n- Untitled group\n  - Passport\n- → then: Leave");
    }

    #[test]
    fn a_copy_carries_what_a_group_encloses() {
        let nodes = vec![
            group("packing", "Packing", 0.0, 0.0, 400.0, 300.0),
            card("passport", "Passport", 50.0, 50.0),
            card("outside", "Elsewhere", 900.0, 0.0),
        ];
        assert_eq!(with_contents(&nodes, &[0]), vec![0, 1]);
        assert_eq!(with_contents(&nodes, &[1, 0, 2]), vec![1, 0, 2], "each once");
    }

    #[test]
    fn the_chains_of_several_cards_share_their_ancestors() {
        let nodes = vec![card("root", "R", 0.0, 0.0), card("a", "A", 300.0, 0.0), card("b", "B", 300.0, 200.0)];
        let edges = vec![edge("root", "a"), edge("root", "b")];
        assert_eq!(upstream(&nodes, &edges, &[1, 2]), vec![0, 1, 2]);
        // A group is itself alone, as Ask AI reads it.
        let nodes = vec![group("g", "G", 0.0, 0.0, 400.0, 300.0), card("before", "B", -900.0, 0.0)];
        assert_eq!(upstream(&nodes, &[edge("before", "g")], &[0]), vec![0]);
    }

    #[test]
    fn html_is_the_markdown_rendered() {
        assert_eq!(html("- **bold**\n- [ ] task"), "<ul>\n<li><strong>bold</strong></li>\n<li><input disabled=\"\" type=\"checkbox\"/>\ntask</li>\n</ul>\n");
    }

    #[test]
    fn a_mermaid_flowchart_has_every_card_group_link_and_colour() {
        let mut nodes = vec![
            card("symptom", "**Login** fails", 0.0, 0.0),
            group("tried", "Tried", 250.0, -50.0, 400.0, 300.0),
            card("skew", "Clock skew\n\n- NTP \"synced\"", 300.0, 0.0),
            // Markdown read as the board draws it: a code span is its text.
            card("next", "C# & 1 > 0 in `tabs`", 900.0, 0.0),
        ];
        nodes[3].color = Some("1".to_owned());
        let mut both = edge("skew", "next");
        both.set_ends(EdgeEnd::Arrow, EdgeEnd::Arrow);
        let edges = vec![labelled("symptom", "skew", "ruled out"), both];
        assert_eq!(
            mermaid(&nodes, &edges, &[0, 1, 2, 3], Direction::Right),
            "```mermaid\n\
             flowchart LR\n    \
                 n1[\"Login fails\"]\n    \
                 n3[\"C#35; #amp; 1 #gt; 0 in tabs\"]\n    \
                 subgraph g1[\"Tried\"]\n        \
                     n2[\"Clock skew<br>• NTP #quot;synced#quot;\"]\n    \
                 end\n    \
                 n1 -->|\"ruled out\"| n2\n    \
                 n2 <--> n3\n    \
                 style n3 fill:#fb464c33,stroke:#fb464c\n\
             ```"
        );
    }

    #[test]
    fn a_mermaid_link_card_can_be_clicked_and_an_arrow_at_the_source_is_turned_round() {
        let mut link = CanvasNode { kind: "link".to_owned(), text: None, x: 300.0, ..node("docs") };
        link.extra.insert("url".to_owned(), json!("https://example.com/a\"b"));
        let nodes = vec![card("q", "Why?", 0.0, 0.0), link];
        let mut back = edge("q", "docs");
        back.set_ends(EdgeEnd::Arrow, EdgeEnd::None);
        let out = mermaid(&nodes, &[back], &[0, 1], Direction::Down);
        assert!(out.starts_with("```mermaid\nflowchart TD\n"), "{out}");
        assert!(out.contains("    n2 --> n1\n"), "{out}");
        assert!(out.contains("    click n2 \"https://example.com/a%22b\"\n"), "{out}");
    }
}
