//! CriticMarkup for tracked changes and comments.
//!
//! Converters record insertions, deletions, highlights and comments as flat
//! tokens in document order. [`Critic::into_inline`] then turns them into
//! markup that nests cleanly, so the simple regular expressions CriticMarkup is
//! designed for can parse it:
//!
//! - a span that crosses another is split so every span closes inside its parent;
//! - a span inside another of the same kind is merged into it (`{--{--x--}--}`
//!   would end at the first `--}`);
//! - spans with no text are dropped, keeping any comments they held;
//! - neighbouring spans of the same kind are joined;
//! - comments inside a highlight move to right after it (`{==text==}{>>note<<}`);
//! - a deletion next to an insertion becomes a substitution (`{~~old~>new~~}`).
//!
//! Document text that happens to contain a CriticMarkup delimiter is escaped
//! with a Markdown backslash (`{\++`), which renders as the original characters
//! but no longer matches the delimiter.

use crate::ooxml::Inline;

/// A CriticMarkup span kind. Moves are an insertion at their destination and a
/// deletion at their source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    Insertion,
    Deletion,
    Highlight,
}

impl Mark {
    pub(crate) fn delimiters(self) -> (&'static str, &'static str) {
        match self {
            Self::Insertion => ("{++", "++}"),
            Self::Deletion => ("{--", "--}"),
            Self::Highlight => ("{==", "==}"),
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Leaf {
    /// Document text: escaped when rendered.
    Text {
        text: String,
        bold: bool,
        italic: bool,
        link: Option<String>,
    },
    /// Markdown built by the converter (images, math, note references).
    Raw(String),
    /// The inside of a `{>>…<<}` comment, already escaped.
    Comment(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Leaf(Leaf),
    Open(Mark),
    Close(Mark),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Node {
    Leaf(Leaf),
    Span(Mark, Vec<Node>),
}

impl Node {
    fn has_content(&self) -> bool {
        match self {
            Self::Leaf(Leaf::Comment(_)) => false,
            Self::Leaf(_) => true,
            Self::Span(_, children) => children.iter().any(Self::has_content),
        }
    }
}

/// Inline content of one paragraph with CriticMarkup spans.
#[derive(Debug, Default)]
pub(crate) struct Critic {
    tokens: Vec<Token>,
}

impl Critic {
    pub(crate) fn push(&mut self, text: &str, bold: bool, italic: bool, link: Option<&str>) {
        if !text.is_empty() {
            self.tokens.push(Token::Leaf(Leaf::Text {
                text: text.to_string(),
                bold,
                italic,
                link: link.map(str::to_string),
            }));
        }
    }

    pub(crate) fn raw(&mut self, markdown: &str) {
        if !markdown.is_empty() {
            self.tokens
                .push(Token::Leaf(Leaf::Raw(markdown.to_string())));
        }
    }

    pub(crate) fn comment(&mut self, note: &str) {
        self.tokens
            .push(Token::Leaf(Leaf::Comment(note.to_string())));
    }

    pub(crate) fn open(&mut self, mark: Mark) {
        self.tokens.push(Token::Open(mark));
    }

    pub(crate) fn close(&mut self, mark: Mark) {
        self.tokens.push(Token::Close(mark));
    }

    /// True when there is no visible text and no comment.
    pub(crate) fn is_blank(&self) -> bool {
        self.tokens.iter().all(|token| match token {
            Token::Leaf(Leaf::Text { text, .. } | Leaf::Raw(text)) => text.trim().is_empty(),
            Token::Leaf(Leaf::Comment(_)) => false,
            Token::Open(_) | Token::Close(_) => true,
        })
    }

    pub(crate) fn into_inline(self) -> Inline {
        let mut inline = Inline::default();
        render(&tidy(tree(self.tokens)), &mut inline);
        inline
    }
}

/// Builds well-nested spans from flat tokens. A span of a kind that is already
/// open is merged into the open one; a close that would cross other spans closes
/// them first and opens them again after it. Spans still open at the end close.
fn tree(tokens: Vec<Token>) -> Vec<Node> {
    let mut root = Vec::new();
    let mut open: Vec<(Mark, Vec<Node>)> = Vec::new();
    let mut merged = [0usize; 3];
    for token in tokens {
        match token {
            Token::Leaf(leaf) => children(&mut root, &mut open).push(Node::Leaf(leaf)),
            Token::Open(mark) if open.iter().any(|(m, _)| *m == mark) => {
                merged[mark.index()] += 1;
            }
            Token::Open(mark) => open.push((mark, Vec::new())),
            Token::Close(mark) if merged[mark.index()] > 0 => merged[mark.index()] -= 1,
            Token::Close(mark) => {
                let Some(at) = open.iter().position(|(m, _)| *m == mark) else {
                    continue;
                };
                let reopen: Vec<Mark> = open[at + 1..].iter().map(|(m, _)| *m).collect();
                close_from(&mut root, &mut open, at);
                open.extend(reopen.into_iter().map(|m| (m, Vec::new())));
            }
        }
    }
    close_from(&mut root, &mut open, 0);
    root
}

fn children<'a>(root: &'a mut Vec<Node>, open: &'a mut [(Mark, Vec<Node>)]) -> &'a mut Vec<Node> {
    match open.last_mut() {
        Some((_, nodes)) => nodes,
        None => root,
    }
}

/// Closes the open spans from index `at` up, innermost first, into their parent.
fn close_from(root: &mut Vec<Node>, open: &mut Vec<(Mark, Vec<Node>)>, at: usize) {
    let closed = open.split_off(at);
    let outermost = closed
        .into_iter()
        .rev()
        .fold(None, |inner, (mark, mut nodes)| {
            nodes.extend(inner);
            Some(Node::Span(mark, nodes))
        });
    children(root, open).extend(outermost);
}

/// Drops empty spans, joins neighbours of the same kind and moves comments out
/// of highlights, at every level.
fn tidy(nodes: Vec<Node>) -> Vec<Node> {
    let mut out: Vec<Node> = Vec::new();
    for node in nodes {
        let Node::Span(mark, nodes) = node else {
            out.push(node);
            continue;
        };
        let mut comments = Vec::new();
        let nodes = if mark == Mark::Highlight {
            lift_comments(nodes, &mut comments)
        } else {
            nodes
        };
        if !nodes.iter().any(Node::has_content) {
            lift_comments(nodes, &mut out);
        } else if let Some(Node::Span(_, before)) = out
            .last_mut()
            .filter(|last| matches!(last, Node::Span(m, _) if *m == mark))
        {
            before.extend(nodes);
        } else {
            out.push(Node::Span(mark, nodes));
        }
        out.extend(comments);
    }
    out.into_iter()
        .map(|node| match node {
            Node::Span(mark, nodes) => Node::Span(mark, tidy(nodes)),
            leaf => leaf,
        })
        .collect()
}

/// Removes every comment below `nodes` into `comments`, in order.
fn lift_comments(nodes: Vec<Node>, comments: &mut Vec<Node>) -> Vec<Node> {
    let mut kept = Vec::new();
    for node in nodes {
        match node {
            Node::Leaf(Leaf::Comment(_)) => comments.push(node),
            Node::Span(mark, nodes) => kept.push(Node::Span(mark, lift_comments(nodes, comments))),
            leaf => kept.push(leaf),
        }
    }
    kept
}

fn render(nodes: &[Node], out: &mut Inline) {
    let mut index = 0;
    while index < nodes.len() {
        match (&nodes[index], nodes.get(index + 1)) {
            (Node::Span(Mark::Deletion, old), Some(Node::Span(Mark::Insertion, new)))
            | (Node::Span(Mark::Insertion, new), Some(Node::Span(Mark::Deletion, old))) => {
                marker(out, "{~~");
                render(old, out);
                marker(out, "~>");
                render(new, out);
                marker(out, "~~}");
                index += 2;
                continue;
            }
            (Node::Span(mark, nodes), _) => {
                let (open, close) = mark.delimiters();
                marker(out, open);
                render(nodes, out);
                marker(out, close);
            }
            (
                Node::Leaf(Leaf::Text {
                    text,
                    bold,
                    italic,
                    link,
                }),
                _,
            ) => {
                out.push(&escape(text), *bold, *italic, link.as_deref());
            }
            (Node::Leaf(Leaf::Raw(markdown)), _) => marker(out, markdown),
            (Node::Leaf(Leaf::Comment(note)), _) => marker(out, &format!("{{>>{note}<<}}")),
        }
        index += 1;
    }
}

fn marker(out: &mut Inline, text: &str) {
    out.push(text, false, false, None);
}

/// Breaks every CriticMarkup delimiter in document text with a Markdown
/// backslash escape, so it renders unchanged but cannot open or close a span.
pub(crate) fn escape(text: &str) -> String {
    const DELIMITERS: [(&str, &str); 11] = [
        ("{++", "{\\++"),
        ("{--", "{\\--"),
        ("{~~", "{\\~~"),
        ("{>>", "{\\>>"),
        ("{==", "{\\=="),
        ("++}", "++\\}"),
        ("--}", "--\\}"),
        ("~~}", "~~\\}"),
        ("<<}", "<<\\}"),
        ("==}", "==\\}"),
        ("~>", "~\\>"),
    ];
    if !text.contains(['{', '}', '~']) {
        return text.to_string();
    }
    DELIMITERS
        .iter()
        .fold(text.to_string(), |text, (from, to)| text.replace(from, to))
}

/// Appends `prefix` and `body` to `out` after `separator`. When the separator is
/// itself tracked (a paragraph mark that was inserted or deleted), it is marked
/// the way the CriticMarkup spec marks a paragraph break (`{++\n\n++}`), and an
/// adjoining span of the same kind is extended rather than opened twice.
pub(crate) fn splice(
    out: &mut String,
    separator: &str,
    prefix: &str,
    body: &str,
    mark: Option<Mark>,
) {
    let Some(mark) = mark else {
        out.push_str(separator);
        out.push_str(prefix);
        out.push_str(body);
        return;
    };
    let (open, close) = mark.delimiters();
    match out.strip_suffix(close) {
        Some(kept) => out.truncate(kept.len()),
        None => out.push_str(open),
    }
    out.push_str(separator);
    out.push_str(prefix);
    match body.strip_prefix(open) {
        Some(rest) => out.push_str(rest),
        None => {
            out.push_str(close);
            out.push_str(body);
        }
    }
}

/// Joins paragraphs, each paired with the revision of the paragraph mark that
/// ends it. A blank paragraph drops the pending mark: its own break stays.
pub(crate) fn join_marked(parts: Vec<(String, Option<Mark>)>, separator: &str) -> String {
    let mut out = String::new();
    let mut pending = None;
    for (part, mark) in parts {
        if part.trim().is_empty() {
            pending = None;
            continue;
        }
        if out.is_empty() {
            out = part;
        } else {
            splice(&mut out, separator, "", &part, pending);
        }
        pending = mark;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use Mark::{Deletion as Del, Highlight as Hl, Insertion as Ins};

    fn text(critic: &mut Critic, t: &str) {
        critic.push(t, false, false, None);
    }

    fn md(critic: Critic) -> String {
        critic.into_inline().render(true)
    }

    /// Builds a paragraph from `(op, value)` steps: `open`/`close` take `+`,
    /// `-` or `=`; `note` is a comment, `raw` raw Markdown, anything else text.
    fn script(steps: &[(&str, &str)]) -> String {
        let mut critic = Critic::default();
        for (op, value) in steps {
            match *op {
                "open" => critic.open(kind(value)),
                "close" => critic.close(kind(value)),
                "note" => critic.comment(value),
                "raw" => critic.raw(value),
                _ => text(&mut critic, value),
            }
        }
        md(critic)
    }

    fn kind(value: &str) -> Mark {
        match value {
            "+" => Ins,
            "-" => Del,
            _ => Hl,
        }
    }

    #[test]
    fn plain_marks_render_with_their_delimiters() {
        assert_eq!(
            script(&[
                ("open", "+"),
                ("t", "a"),
                ("close", "+"),
                ("t", " "),
                ("open", "="),
                ("t", "b"),
                ("close", "=")
            ]),
            "{++a++} {==b==}"
        );
        assert_eq!(
            script(&[("open", "-"), ("t", "gone"), ("close", "-")]),
            "{--gone--}"
        );
    }

    #[test]
    fn deletion_next_to_insertion_becomes_a_substitution_in_either_order() {
        let del_ins = [
            ("open", "-"),
            ("t", "old"),
            ("close", "-"),
            ("open", "+"),
            ("t", "new"),
            ("close", "+"),
        ];
        let ins_del = [
            ("open", "+"),
            ("t", "new"),
            ("close", "+"),
            ("open", "-"),
            ("t", "old"),
            ("close", "-"),
        ];
        assert_eq!(script(&del_ins), "{~~old~>new~~}");
        assert_eq!(script(&ins_del), "{~~old~>new~~}");
        // Only one pair: a third span stays on its own.
        let triple = [
            ("open", "-"),
            ("t", "a"),
            ("close", "-"),
            ("open", "+"),
            ("t", "b"),
            ("close", "+"),
            ("open", "-"),
            ("t", "c"),
            ("close", "-"),
        ];
        assert_eq!(script(&triple), "{~~a~>b~~}{--c--}");
    }

    #[test]
    fn same_kind_nesting_is_merged_into_one_span() {
        let nested = [
            ("open", "-"),
            ("t", "a"),
            ("open", "-"),
            ("t", "b"),
            ("close", "-"),
            ("t", "c"),
            ("close", "-"),
            ("t", "d"),
        ];
        // The outer close still ends the span: `d` is outside it.
        assert_eq!(script(&nested), "{--abc--}d");
    }

    #[test]
    fn merged_spans_are_counted_per_kind() {
        // Two overlapping highlights, and an insertion inside them: closing the
        // insertion must not use up the highlights' count.
        let steps = [
            ("open", "="),
            ("open", "="),
            ("t", "a"),
            ("open", "+"),
            ("t", "b"),
            ("close", "+"),
            ("t", "c"),
            ("close", "="),
            ("close", "="),
        ];
        assert_eq!(script(&steps), "{==a{++b++}c==}");
    }

    #[test]
    fn different_kinds_may_nest() {
        // Inserted by one author, deleted by another: gone either way.
        let nested = [
            ("open", "+"),
            ("open", "-"),
            ("t", "x"),
            ("close", "-"),
            ("close", "+"),
        ];
        assert_eq!(script(&nested), "{++{--x--}++}");
    }

    #[test]
    fn crossing_spans_are_split_so_each_closes_inside_its_parent() {
        let crossing = [
            ("open", "="),
            ("t", "a"),
            ("open", "+"),
            ("t", "b"),
            ("close", "="),
            ("t", "c"),
            ("close", "+"),
        ];
        assert_eq!(script(&crossing), "{==a{++b++}==}{++c++}");
    }

    #[test]
    fn neighbours_of_the_same_kind_join() {
        let split = [
            ("open", "+"),
            ("t", "a"),
            ("close", "+"),
            ("open", "+"),
            ("t", "b"),
            ("close", "+"),
        ];
        assert_eq!(script(&split), "{++ab++}");
        // Joining exposes inner neighbours, which join too.
        let deep = [
            ("open", "-"),
            ("open", "+"),
            ("t", "a"),
            ("close", "+"),
            ("close", "-"),
            ("open", "-"),
            ("open", "+"),
            ("t", "b"),
            ("close", "+"),
            ("close", "-"),
        ];
        assert_eq!(script(&deep), "{--{++ab++}--}");
    }

    #[test]
    fn empty_spans_vanish_but_keep_their_comments() {
        assert_eq!(
            script(&[("t", "a"), ("open", "+"), ("close", "+"), ("t", "b")]),
            "ab"
        );
        assert_eq!(
            script(&[
                ("t", "a"),
                ("open", "+"),
                ("open", "-"),
                ("note", "c"),
                ("close", "-"),
                ("close", "+")
            ]),
            "a{>>c<<}"
        );
        // A whitespace insertion is real content.
        assert_eq!(
            script(&[
                ("t", "a"),
                ("open", "+"),
                ("t", " "),
                ("close", "+"),
                ("t", "b")
            ]),
            "a{++ ++}b"
        );
    }

    #[test]
    fn comments_inside_a_highlight_follow_it() {
        let steps = [
            ("open", "="),
            ("t", "a"),
            ("note", "one"),
            ("open", "+"),
            ("t", "b"),
            ("note", "two"),
            ("close", "+"),
            ("close", "="),
            ("t", "."),
        ];
        assert_eq!(script(&steps), "{==a{++b++}==}{>>one<<}{>>two<<}.");
        // Two highlights separated by a comment stay separate.
        let steps = [
            ("open", "="),
            ("t", "a"),
            ("close", "="),
            ("note", "x"),
            ("open", "="),
            ("t", "b"),
            ("close", "="),
        ];
        assert_eq!(script(&steps), "{==a==}{>>x<<}{==b==}");
    }

    #[test]
    fn overlapping_highlights_become_their_union() {
        let steps = [
            ("open", "="),
            ("t", "a"),
            ("open", "="),
            ("t", "b"),
            ("close", "="),
            ("note", "A"),
            ("t", "c"),
            ("close", "="),
            ("note", "B"),
        ];
        assert_eq!(script(&steps), "{==abc==}{>>A<<}{>>B<<}");
    }

    #[test]
    fn unclosed_spans_close_at_the_end_and_stray_closes_are_ignored() {
        assert_eq!(
            script(&[("close", "+"), ("open", "-"), ("t", "a")]),
            "{--a--}"
        );
    }

    #[test]
    fn raw_markdown_is_content_but_not_escaped() {
        assert_eq!(
            script(&[
                ("open", "+"),
                ("raw", "$x_{--}$"),
                ("close", "+"),
                ("raw", "")
            ]),
            "{++$x_{--}$++}"
        );
    }

    #[test]
    fn formatting_and_links_stay_inside_the_markers() {
        let mut critic = Critic::default();
        critic.open(Ins);
        critic.push("bold", true, false, None);
        critic.close(Ins);
        critic.push(" ", false, false, None);
        critic.open(Del);
        critic.push("site", false, false, Some("https://x.test"));
        critic.close(Del);
        critic.push("", false, false, None);
        assert_eq!(md(critic), "{++**bold**++} {--[site](https://x.test)--}");
    }

    #[test]
    fn blankness_ignores_markers_but_not_comments() {
        let mut critic = Critic::default();
        critic.open(Ins);
        text(&mut critic, "  ");
        critic.raw(" ");
        critic.close(Ins);
        assert!(critic.is_blank());
        critic.comment("note");
        assert!(!critic.is_blank());
        let mut raw = Critic::default();
        raw.raw("![x](y)");
        assert!(!raw.is_blank());
    }

    #[test]
    fn delimiters_in_document_text_are_escaped() {
        assert_eq!(escape("plain"), "plain");
        assert_eq!(escape("a{b}c"), "a{b}c");
        assert_eq!(
            escape("{++ {-- {~~ {>> {== ++} --} ~~} <<} ==} ~>"),
            "{\\++ {\\-- {\\~~ {\\>> {\\== ++\\} --\\} ~~\\} <<\\} ==\\} ~\\>"
        );
        assert_eq!(escape("{+++}"), "{\\+++\\}");
        assert_eq!(
            script(&[("open", "+"), ("t", "a --} b"), ("close", "+")]),
            "{++a --\\} b++}"
        );
    }

    #[test]
    fn splice_marks_a_tracked_break_and_extends_adjacent_spans() {
        let join = |out: &str, body: &str, mark| {
            let mut out = out.to_string();
            splice(&mut out, "\n\n", "", body, mark);
            out
        };
        assert_eq!(join("A", "B", None), "A\n\nB");
        assert_eq!(join("A", "B", Some(Del)), "A{--\n\n--}B");
        assert_eq!(join("A", "B", Some(Ins)), "A{++\n\n++}B");
        assert_eq!(join("{--A--}", "{--B--}", Some(Del)), "{--A\n\nB--}");
        assert_eq!(
            join("Keep {--old--}", "B", Some(Del)),
            "Keep {--old\n\n--}B"
        );
        assert_eq!(join("A", "{++B++} rest", Some(Ins)), "A{++\n\nB++} rest");
        // An escaped delimiter in the text is not a span to extend.
        assert_eq!(join("a --\\}", "b", Some(Del)), "a --\\}{--\n\n--}b");
        let mut heading = "A".to_string();
        splice(&mut heading, "\n\n", "## ", "B", Some(Ins));
        assert_eq!(heading, "A{++\n\n## ++}B");
    }

    #[test]
    fn join_marked_uses_each_paragraph_mark_and_resets_on_blanks() {
        let parts = vec![
            ("a".to_string(), Some(Del)),
            ("b".to_string(), None),
            ("c".to_string(), Some(Ins)),
            ("  ".to_string(), Some(Del)),
            ("d".to_string(), Some(Del)),
        ];
        assert_eq!(join_marked(parts, " "), "a{-- --}b c d");
        assert_eq!(join_marked(Vec::new(), " "), "");
    }

    #[test]
    fn tree_ignores_a_close_with_nothing_open() {
        assert_eq!(tree(vec![Token::Close(Ins)]), Vec::<Node>::new());
    }
}
