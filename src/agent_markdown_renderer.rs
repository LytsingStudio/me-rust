use std::{cmp, ops::Range, str::FromStr, sync::OnceLock};

use pulldown_cmark::{Alignment, BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, Tag};
use two_face::re_exports::syntect::{
    easy::ScopeRegionIterator,
    highlighting::ScopeSelectors,
    parsing::{ParseState, ScopeStack, SyntaxSet},
    util::LinesWithEndings,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::markdown;

pub const COMPONENT_NAME: &str = "agent-markdown-renderer";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorRole {
    #[default]
    Primary,
    Muted,
    Accent,
    Link,
    Code,
    SyntaxComment,
    SyntaxString,
    SyntaxKeyword,
    SyntaxDeclaration,
    SyntaxNumber,
    SyntaxType,
    SyntaxFunction,
    SyntaxVariable,
    SyntaxConstant,
    Math,
    Success,
    Warning,
    Error,
    Border,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextStyle {
    pub color: ColorRole,
    pub bold: bool,
    pub italic: bool,
    pub underlined: bool,
    pub crossed_out: bool,
    pub dim: bool,
}

impl TextStyle {
    pub fn colored(color: ColorRole) -> Self {
        Self {
            color,
            ..Self::default()
        }
    }

    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    fn merged(self, overlay: Self) -> Self {
        Self {
            color: if overlay.color == ColorRole::Primary {
                self.color
            } else {
                overlay.color
            },
            bold: self.bold || overlay.bold,
            italic: self.italic || overlay.italic,
            underlined: self.underlined || overlay.underlined,
            crossed_out: self.crossed_out || overlay.crossed_out,
            dim: self.dim || overlay.dim,
        }
    }
}

struct SyntaxSelectors {
    comment: ScopeSelectors,
    string: ScopeSelectors,
    number: ScopeSelectors,
    declaration: ScopeSelectors,
    keyword: ScopeSelectors,
    type_name: ScopeSelectors,
    function: ScopeSelectors,
    variable: ScopeSelectors,
    constant: ScopeSelectors,
}

impl SyntaxSelectors {
    fn new() -> Self {
        Self {
            comment: parse_scope_selector("comment"),
            string: parse_scope_selector("string, constant.other.symbol"),
            number: parse_scope_selector("constant.numeric"),
            declaration: parse_scope_selector("storage, keyword.declaration, entity.name.tag"),
            keyword: parse_scope_selector("keyword - keyword.operator"),
            type_name: parse_scope_selector(
                "entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, \
                 support.type, support.class",
            ),
            function: parse_scope_selector(
                "entity.name.function, support.function, variable.function",
            ),
            variable: parse_scope_selector(
                "entity.other.attribute-name, support.type.property-name, variable",
            ),
            constant: parse_scope_selector("constant, support.constant"),
        }
    }

    fn style(&self, stack: &ScopeStack) -> TextStyle {
        let scopes = stack.as_slice();
        let role = [
            (&self.constant, ColorRole::SyntaxConstant),
            (&self.variable, ColorRole::SyntaxVariable),
            (&self.function, ColorRole::SyntaxFunction),
            (&self.type_name, ColorRole::SyntaxType),
            (&self.keyword, ColorRole::SyntaxKeyword),
            (&self.declaration, ColorRole::SyntaxDeclaration),
            (&self.number, ColorRole::SyntaxNumber),
            (&self.string, ColorRole::SyntaxString),
            (&self.comment, ColorRole::SyntaxComment),
        ]
        .into_iter()
        .filter_map(|(selector, role)| selector.does_match(scopes).map(|power| (power, role)))
        .max_by_key(|(power, _)| *power)
        .map(|(_, role)| role)
        .unwrap_or(ColorRole::Primary);
        TextStyle::colored(role)
    }
}

fn parse_scope_selector(selector: &str) -> ScopeSelectors {
    ScopeSelectors::from_str(selector).expect("built-in syntax selector must be valid")
}

fn syntax_set() -> &'static SyntaxSet {
    static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAX_SET.get_or_init(two_face::syntax::extra_newlines)
}

fn syntax_selectors() -> &'static SyntaxSelectors {
    static SELECTORS: OnceLock<SyntaxSelectors> = OnceLock::new();
    SELECTORS.get_or_init(SyntaxSelectors::new)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyledSpan {
    pub text: String,
    pub style: TextStyle,
}

impl StyledSpan {
    pub fn new(text: impl Into<String>, style: TextStyle) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }

    pub fn width(&self) -> usize {
        display_width(&self.text)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StyledLine {
    pub spans: Vec<StyledSpan>,
}

impl StyledLine {
    pub fn plain_text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }

    pub fn width(&self) -> usize {
        self.spans.iter().map(StyledSpan::width).sum()
    }

    fn push(&mut self, text: impl Into<String>, style: TextStyle) {
        let text = text.into();
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.spans.last_mut().filter(|last| last.style == style) {
            last.text.push_str(&text);
        } else {
            self.spans.push(StyledSpan::new(text, style));
        }
    }

    fn prepend(&mut self, text: impl Into<String>, style: TextStyle) {
        let text = text.into();
        if text.is_empty() {
            return;
        }
        if let Some(first) = self.spans.first_mut().filter(|first| first.style == style) {
            first.text.insert_str(0, &text);
        } else {
            self.spans.insert(0, StyledSpan::new(text, style));
        }
    }

    fn append_line(&mut self, other: StyledLine) {
        for span in other.spans {
            self.push(span.text, span.style);
        }
    }
}

#[derive(Clone, Debug)]
enum Node {
    Element {
        tag: Tag<'static>,
        children: Vec<Node>,
    },
    Text(String),
    Code(String),
    InlineMath(String),
    DisplayMath(String),
    Html(String),
    InlineHtml(String),
    FootnoteReference(String),
    SoftBreak,
    HardBreak,
    Rule,
    TaskListMarker(bool),
}

#[derive(Clone, Debug)]
enum InlinePiece {
    Span(StyledSpan),
    SoftBreak,
    HardBreak,
}

#[derive(Clone, Debug)]
struct Unit {
    text: String,
    style: TextStyle,
    width: usize,
}

#[derive(Clone, Debug)]
enum InlineToken {
    Word(Vec<Unit>),
    Space(TextStyle),
    HardBreak,
}

pub fn render(markdown: &str, width: usize) -> Vec<StyledLine> {
    let width = width.max(1);
    let document = parse_document(markdown);
    let renderer = Renderer { width };
    renderer
        .render_blocks(&document)
        .into_iter()
        .map(|line| clip_line(line, width))
        .collect()
}

pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn parse_document(source: &str) -> Vec<Node> {
    let mut root = Vec::new();
    let mut stack: Vec<(Tag<'static>, Vec<Node>)> = Vec::new();
    let source = markdown::normalize_cjk_emphasis(source);

    for event in markdown::parser(source.as_ref()) {
        match event {
            Event::Start(tag) => stack.push((tag.into_static(), Vec::new())),
            Event::End(actual) => {
                let Some((tag, children)) = stack.pop() else {
                    continue;
                };
                debug_assert_eq!(tag.to_end(), actual);
                push_node(&mut root, &mut stack, Node::Element { tag, children });
            }
            Event::Text(text) => push_node(
                &mut root,
                &mut stack,
                Node::Text(clean_multiline_text(text.as_ref())),
            ),
            Event::Code(text) => {
                push_node(&mut root, &mut stack, Node::Code(clean_text(text.as_ref())))
            }
            Event::InlineMath(text) => push_node(
                &mut root,
                &mut stack,
                Node::InlineMath(clean_text(text.as_ref())),
            ),
            Event::DisplayMath(text) => push_node(
                &mut root,
                &mut stack,
                Node::DisplayMath(clean_text(text.as_ref())),
            ),
            Event::Html(text) => push_node(
                &mut root,
                &mut stack,
                Node::Html(clean_multiline_text(text.as_ref())),
            ),
            Event::InlineHtml(text) => push_node(
                &mut root,
                &mut stack,
                Node::InlineHtml(clean_text(text.as_ref())),
            ),
            Event::FootnoteReference(label) => push_node(
                &mut root,
                &mut stack,
                Node::FootnoteReference(clean_text(label.as_ref())),
            ),
            Event::SoftBreak => push_node(&mut root, &mut stack, Node::SoftBreak),
            Event::HardBreak => push_node(&mut root, &mut stack, Node::HardBreak),
            Event::Rule => push_node(&mut root, &mut stack, Node::Rule),
            Event::TaskListMarker(checked) => {
                push_node(&mut root, &mut stack, Node::TaskListMarker(checked));
            }
        }
    }

    while let Some((tag, children)) = stack.pop() {
        push_node(&mut root, &mut stack, Node::Element { tag, children });
    }
    root
}

fn push_node(root: &mut Vec<Node>, stack: &mut [(Tag<'static>, Vec<Node>)], node: Node) {
    if let Some((_, children)) = stack.last_mut() {
        children.push(node);
    } else {
        root.push(node);
    }
}

fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .collect()
}

fn clean_multiline_text(text: &str) -> String {
    text.chars()
        .filter(|character| matches!(character, '\n' | '\r' | '\t') || !character.is_control())
        .collect()
}

struct Renderer {
    width: usize,
}

impl Renderer {
    fn render_blocks(&self, nodes: &[Node]) -> Vec<StyledLine> {
        let mut output = Vec::new();
        let mut inline_fallback = Vec::new();

        for node in nodes {
            let block = match node {
                Node::Element { tag, children } => match tag {
                    Tag::Paragraph => Some(match children.as_slice() {
                        [Node::DisplayMath(formula)] => self.render_display_math(formula),
                        _ => self.render_inline_block(children, TextStyle::default()),
                    }),
                    Tag::Heading { level, .. } => Some(self.render_heading(*level, children)),
                    Tag::BlockQuote(kind) => Some(self.render_quote(*kind, children)),
                    Tag::CodeBlock(kind) => Some(self.render_code_block(kind, children)),
                    Tag::List(start) => Some(self.render_list(*start, children)),
                    Tag::FootnoteDefinition(label) => {
                        Some(self.render_footnote(label.as_ref(), children))
                    }
                    Tag::DefinitionList => Some(self.render_definition_list(children)),
                    Tag::Table(alignments) => Some(self.render_table(alignments, children)),
                    Tag::HtmlBlock => Some(self.render_html_block(children)),
                    Tag::Item
                    | Tag::DefinitionListTitle
                    | Tag::DefinitionListDefinition
                    | Tag::TableHead
                    | Tag::TableRow
                    | Tag::TableCell => Some(self.render_blocks(children)),
                    Tag::Emphasis
                    | Tag::Strong
                    | Tag::Strikethrough
                    | Tag::Superscript
                    | Tag::Subscript
                    | Tag::Link { .. }
                    | Tag::Image { .. }
                    | Tag::MetadataBlock(_) => {
                        inline_fallback.push(node.clone());
                        None
                    }
                },
                Node::Rule => Some(vec![StyledLine {
                    spans: vec![StyledSpan::new(
                        "─".repeat(self.width),
                        TextStyle::colored(ColorRole::Border),
                    )],
                }]),
                Node::DisplayMath(formula) => Some(self.render_display_math(formula)),
                Node::Html(html) => Some(self.render_raw_block(html, ColorRole::Muted)),
                _ => {
                    inline_fallback.push(node.clone());
                    None
                }
            };

            if let Some(block) = block {
                if !inline_fallback.is_empty() {
                    let fallback = self.render_inline_block(&inline_fallback, TextStyle::default());
                    append_block(&mut output, fallback);
                    inline_fallback.clear();
                }
                append_block(&mut output, block);
            }
        }
        if !inline_fallback.is_empty() {
            append_block(
                &mut output,
                self.render_inline_block(&inline_fallback, TextStyle::default()),
            );
        }
        trim_blank_lines(&mut output);
        output
    }

    fn render_inline_block(&self, nodes: &[Node], style: TextStyle) -> Vec<StyledLine> {
        wrap_inline(&inline_pieces(nodes, style), self.width)
    }

    fn render_heading(&self, level: HeadingLevel, children: &[Node]) -> Vec<StyledLine> {
        let style = TextStyle {
            bold: true,
            underlined: level == HeadingLevel::H1,
            ..TextStyle::default()
        };
        self.render_inline_block(children, style)
    }

    fn render_quote(&self, kind: Option<BlockQuoteKind>, children: &[Node]) -> Vec<StyledLine> {
        let inner_width = self.width.saturating_sub(2).max(1);
        let mut lines = Renderer { width: inner_width }.render_blocks(children);
        if let Some(kind) = kind {
            let label = match kind {
                BlockQuoteKind::Note => "NOTE",
                BlockQuoteKind::Tip => "TIP",
                BlockQuoteKind::Important => "IMPORTANT",
                BlockQuoteKind::Warning => "WARNING",
                BlockQuoteKind::Caution => "CAUTION",
            };
            let role = match kind {
                BlockQuoteKind::Tip => ColorRole::Success,
                BlockQuoteKind::Warning => ColorRole::Warning,
                BlockQuoteKind::Caution => ColorRole::Error,
                BlockQuoteKind::Note | BlockQuoteKind::Important => ColorRole::Accent,
            };
            lines.insert(
                0,
                StyledLine {
                    spans: vec![StyledSpan::new(label, TextStyle::colored(role).bold())],
                },
            );
        }
        if lines.is_empty() {
            lines.push(StyledLine::default());
        }
        for line in &mut lines {
            line.prepend("│ ", TextStyle::colored(ColorRole::Accent));
        }
        lines
    }

    fn render_code_block(&self, kind: &CodeBlockKind<'_>, children: &[Node]) -> Vec<StyledLine> {
        let language = match kind {
            CodeBlockKind::Indented => "",
            CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or_default(),
        };
        let mut content = String::new();
        collect_literal(children, &mut content);
        let border = TextStyle::colored(ColorRole::Border);
        let fallback = TextStyle::default();

        let mut lines = Vec::new();
        let label = if language.is_empty() {
            "╭".to_owned()
        } else {
            format!("╭─ {language} ")
        };
        let label = truncate_plain(&label, self.width.saturating_sub(1).max(1));
        let mut top = StyledLine::default();
        top.push(&label, border);
        top.push(
            "─".repeat(self.width.saturating_sub(top.width()).saturating_sub(1)),
            border,
        );
        if self.width > 1 {
            top.push("╮", border);
        }
        lines.push(top);

        let content = content.trim_end_matches(['\r', '\n']);
        let horizontal_padding = (self.width.saturating_sub(1) / 2).min(2);
        let inner_width = self
            .width
            .saturating_sub(horizontal_padding.saturating_mul(2))
            .max(1);
        let content_lines = if content.is_empty() {
            vec![StyledLine::default()]
        } else {
            highlight_code(content, language, inner_width)
                .unwrap_or_else(|| wrap_preserving_whitespace(content, inner_width, fallback))
        };
        for mut line in content_lines {
            if horizontal_padding > 0 {
                line.prepend(" ".repeat(horizontal_padding), TextStyle::default());
            }
            lines.push(line);
        }

        let mut bottom = StyledLine::default();
        bottom.push("╰", border);
        bottom.push("─".repeat(self.width.saturating_sub(2)), border);
        if self.width > 1 {
            bottom.push("╯", border);
        }
        lines.push(bottom);
        lines
    }

    fn render_list(&self, start: Option<u64>, children: &[Node]) -> Vec<StyledLine> {
        let items = children
            .iter()
            .filter_map(|node| match node {
                Node::Element {
                    tag: Tag::Item,
                    children,
                } => Some(children.as_slice()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut output = Vec::new();
        for (index, item) in items.into_iter().enumerate() {
            let marker = start.map_or_else(
                || "• ".to_owned(),
                |start| format!("{}. ", start.saturating_add(index as u64)),
            );
            let marker_width = display_width(&marker);
            let inner_width = self.width.saturating_sub(marker_width).max(1);
            let mut item_lines = Renderer { width: inner_width }.render_blocks(item);
            if item_lines.is_empty() {
                item_lines.push(StyledLine::default());
            }
            for (line_index, mut line) in item_lines.into_iter().enumerate() {
                if line_index == 0 {
                    line.prepend(&marker, TextStyle::colored(ColorRole::Muted));
                } else {
                    line.prepend(" ".repeat(marker_width), TextStyle::default());
                }
                output.push(line);
            }
        }
        output
    }

    fn render_footnote(&self, label: &str, children: &[Node]) -> Vec<StyledLine> {
        let prefix = format!("[{label}] ");
        self.render_prefixed_container(
            &prefix,
            TextStyle::colored(ColorRole::Accent).bold(),
            children,
        )
    }

    fn render_definition_list(&self, children: &[Node]) -> Vec<StyledLine> {
        let mut output = Vec::new();
        for child in children {
            match child {
                Node::Element {
                    tag: Tag::DefinitionListTitle,
                    children,
                } => {
                    let lines = Renderer { width: self.width }
                        .render_inline_block(children, TextStyle::default().bold());
                    append_block_without_gap(&mut output, lines);
                }
                Node::Element {
                    tag: Tag::DefinitionListDefinition,
                    children,
                } => {
                    let lines = self.render_prefixed_container(
                        "  · ",
                        TextStyle::colored(ColorRole::Accent),
                        children,
                    );
                    append_block_without_gap(&mut output, lines);
                }
                _ => {}
            }
        }
        output
    }

    fn render_prefixed_container(
        &self,
        prefix: &str,
        prefix_style: TextStyle,
        children: &[Node],
    ) -> Vec<StyledLine> {
        let prefix_width = display_width(prefix);
        let inner_width = self.width.saturating_sub(prefix_width).max(1);
        let mut lines = Renderer { width: inner_width }.render_blocks(children);
        if lines.is_empty() {
            lines.push(StyledLine::default());
        }
        for (index, line) in lines.iter_mut().enumerate() {
            if index == 0 {
                line.prepend(prefix, prefix_style);
            } else {
                line.prepend(" ".repeat(prefix_width), TextStyle::default());
            }
        }
        lines
    }

    fn render_display_math(&self, formula: &str) -> Vec<StyledLine> {
        let style = TextStyle::colored(ColorRole::Math);
        if self.width < 3 {
            return wrap_preserving_whitespace(formula, self.width, style);
        }
        let inner_width = self.width.saturating_sub(2).max(1);
        let mut lines = wrap_preserving_whitespace(formula, inner_width, style);
        for line in &mut lines {
            line.prepend("│ ", TextStyle::colored(ColorRole::Math));
        }
        lines
    }

    fn render_html_block(&self, children: &[Node]) -> Vec<StyledLine> {
        let mut html = String::new();
        collect_literal(children, &mut html);
        self.render_raw_block(&html, ColorRole::Muted)
    }

    fn render_raw_block(&self, text: &str, role: ColorRole) -> Vec<StyledLine> {
        wrap_preserving_whitespace(text, self.width, TextStyle::colored(role))
    }

    fn render_table(&self, alignments: &[Alignment], children: &[Node]) -> Vec<StyledLine> {
        let Some(table) = TableData::from_nodes(alignments, children) else {
            return Vec::new();
        };
        let minimum_width = table.column_count.saturating_mul(12).saturating_add(1);
        if self.width < minimum_width {
            return self.render_stacked_table(&table);
        }

        let widths = table.column_widths(self.width);
        let border = TextStyle::colored(ColorRole::Border);
        let mut output = Vec::new();
        output.push(table_border('┌', '┬', '┐', &widths, border));
        for (row_index, row) in table.rows.iter().enumerate() {
            let header = row_index == 0 && table.has_header;
            let rendered_cells = (0..table.column_count)
                .map(|column| {
                    let pieces = row.get(column).cloned().unwrap_or_default();
                    let pieces = if header {
                        apply_style(
                            pieces,
                            TextStyle {
                                bold: true,
                                ..TextStyle::default()
                            },
                        )
                    } else {
                        pieces
                    };
                    wrap_inline(&pieces, widths[column])
                })
                .collect::<Vec<_>>();
            let height = rendered_cells
                .iter()
                .map(Vec::len)
                .max()
                .unwrap_or(1)
                .max(1);
            for line_index in 0..height {
                let mut line = StyledLine::default();
                line.push("│", border);
                for column in 0..table.column_count {
                    line.push(" ", TextStyle::default());
                    let cell = rendered_cells[column]
                        .get(line_index)
                        .cloned()
                        .unwrap_or_default();
                    line.append_line(align_line(cell, widths[column], table.alignments[column]));
                    line.push(" ", TextStyle::default());
                    line.push("│", border);
                }
                output.push(line);
            }
            if row_index + 1 < table.rows.len() {
                output.push(table_border('├', '┼', '┤', &widths, border));
            }
        }
        output.push(table_border('└', '┴', '┘', &widths, border));
        output
    }

    fn render_stacked_table(&self, table: &TableData) -> Vec<StyledLine> {
        if self.width < 4 {
            let mut output = Vec::new();
            for row in &table.rows {
                for cell in row {
                    append_block_without_gap(&mut output, wrap_inline(cell, self.width));
                }
            }
            return output;
        }

        let border = TextStyle::colored(ColorRole::Border);
        let inner_width = self.width.saturating_sub(4).max(1);
        let headers = table
            .rows
            .first()
            .filter(|_| table.has_header && table.rows.len() > 1)
            .cloned()
            .unwrap_or_default();
        let body = if table.has_header && table.rows.len() > 1 {
            &table.rows[1..]
        } else {
            &table.rows[..]
        };
        let mut output = vec![single_box_border('┌', '┐', self.width, border)];
        for (row_index, row) in body.iter().enumerate() {
            for column in 0..table.column_count {
                let mut pieces = Vec::new();
                if let Some(header) = headers.get(column) {
                    pieces.extend(apply_style(
                        header.clone(),
                        TextStyle {
                            bold: true,
                            ..TextStyle::default()
                        },
                    ));
                    pieces.push(InlinePiece::Span(StyledSpan::new(
                        ": ",
                        TextStyle::colored(ColorRole::Muted),
                    )));
                }
                pieces.extend(row.get(column).cloned().unwrap_or_default());
                for cell_line in wrap_inline(&pieces, inner_width) {
                    let mut line = StyledLine::default();
                    line.push("│ ", border);
                    line.append_line(pad_line_right(cell_line, inner_width));
                    line.push(" │", border);
                    output.push(line);
                }
            }
            if row_index + 1 < body.len() {
                output.push(single_box_border('├', '┤', self.width, border));
            }
        }
        output.push(single_box_border('└', '┘', self.width, border));
        output
    }
}

struct TableData {
    alignments: Vec<Alignment>,
    rows: Vec<Vec<Vec<InlinePiece>>>,
    column_count: usize,
    has_header: bool,
}

impl TableData {
    fn from_nodes(alignments: &[Alignment], children: &[Node]) -> Option<Self> {
        let mut rows = Vec::new();
        let mut has_header = false;
        for child in children {
            match child {
                Node::Element {
                    tag: Tag::TableHead,
                    children,
                } => {
                    has_header = true;
                    rows.push(table_cells(children));
                }
                Node::Element {
                    tag: Tag::TableRow,
                    children,
                } => rows.push(table_cells(children)),
                _ => {}
            }
        }
        let column_count = cmp::max(
            alignments.len(),
            rows.iter().map(Vec::len).max().unwrap_or(0),
        );
        if column_count == 0 {
            return None;
        }
        let mut normalized_alignments = alignments.to_vec();
        normalized_alignments.resize(column_count, Alignment::None);
        Some(Self {
            alignments: normalized_alignments,
            rows,
            column_count,
            has_header,
        })
    }

    fn column_widths(&self, total_width: usize) -> Vec<usize> {
        let border_width = self.column_count.saturating_mul(3).saturating_add(1);
        let budget = total_width.saturating_sub(border_width);
        let minimum = if budget >= self.column_count.saturating_mul(3) {
            3
        } else {
            1
        };
        let mut widths = vec![minimum; self.column_count];
        let desired = (0..self.column_count)
            .map(|column| {
                self.rows
                    .iter()
                    .filter_map(|row| row.get(column))
                    .map(|cell| natural_inline_width(cell))
                    .max()
                    .unwrap_or(1)
                    .max(minimum)
            })
            .collect::<Vec<_>>();
        let desired_budget = desired
            .iter()
            .fold(0usize, |total, width| total.saturating_add(*width));
        let target_budget = budget.min(desired_budget);
        let mut remaining = target_budget.saturating_sub(minimum.saturating_mul(self.column_count));
        while remaining > 0 {
            let Some(column) = (0..self.column_count)
                .filter(|column| widths[*column] < desired[*column])
                .max_by_key(|column| desired[*column].saturating_sub(widths[*column]))
            else {
                break;
            };
            widths[column] += 1;
            remaining -= 1;
        }
        widths
    }
}

fn table_cells(nodes: &[Node]) -> Vec<Vec<InlinePiece>> {
    nodes
        .iter()
        .filter_map(|node| match node {
            Node::Element {
                tag: Tag::TableCell,
                children,
            } => Some(inline_pieces(children, TextStyle::default())),
            _ => None,
        })
        .collect()
}

fn inline_pieces(nodes: &[Node], style: TextStyle) -> Vec<InlinePiece> {
    let mut pieces = Vec::new();
    for node in nodes {
        match node {
            Node::Element { tag, children } => match tag {
                Tag::Emphasis => extend_inline(
                    &mut pieces,
                    inline_pieces(
                        children,
                        style.merged(TextStyle {
                            italic: true,
                            ..TextStyle::default()
                        }),
                    ),
                ),
                Tag::Strong => extend_inline(
                    &mut pieces,
                    inline_pieces(
                        children,
                        style.merged(TextStyle {
                            bold: true,
                            ..TextStyle::default()
                        }),
                    ),
                ),
                Tag::Strikethrough => extend_inline(
                    &mut pieces,
                    inline_pieces(
                        children,
                        style.merged(TextStyle {
                            crossed_out: true,
                            ..TextStyle::default()
                        }),
                    ),
                ),
                Tag::Superscript | Tag::Subscript => extend_inline(
                    &mut pieces,
                    inline_pieces(
                        children,
                        style.merged(TextStyle {
                            italic: true,
                            dim: true,
                            ..TextStyle::default()
                        }),
                    ),
                ),
                Tag::Link { .. } => extend_inline(
                    &mut pieces,
                    inline_pieces(
                        children,
                        style.merged(TextStyle {
                            color: ColorRole::Link,
                            underlined: true,
                            ..TextStyle::default()
                        }),
                    ),
                ),
                Tag::Image { dest_url, .. } => {
                    let alt = inline_plain_text(children);
                    let label = if alt.trim().is_empty() {
                        format!("▧ {}", dest_url)
                    } else {
                        format!("▧ {alt}")
                    };
                    push_inline_span(
                        &mut pieces,
                        label,
                        style.merged(TextStyle {
                            color: ColorRole::Link,
                            underlined: true,
                            ..TextStyle::default()
                        }),
                    );
                }
                _ => extend_inline(&mut pieces, inline_pieces(children, style)),
            },
            Node::Text(text) => {
                if matches!(style.color, ColorRole::Link | ColorRole::Code) {
                    push_inline_span(&mut pieces, text, style);
                } else {
                    push_text_with_bare_urls(&mut pieces, text, style);
                }
            }
            Node::Code(code) => push_inline_span(
                &mut pieces,
                code,
                style.merged(TextStyle::colored(ColorRole::Code)),
            ),
            Node::InlineMath(formula) => push_inline_span(
                &mut pieces,
                formula,
                style.merged(TextStyle::colored(ColorRole::Math)),
            ),
            Node::DisplayMath(formula) => push_inline_span(
                &mut pieces,
                formula,
                style.merged(TextStyle::colored(ColorRole::Math)),
            ),
            Node::InlineHtml(html) => {
                if is_html_break(html) {
                    pieces.push(InlinePiece::HardBreak);
                }
            }
            Node::Html(html) => push_inline_span(
                &mut pieces,
                html,
                style.merged(TextStyle::colored(ColorRole::Muted)),
            ),
            Node::FootnoteReference(label) => push_inline_span(
                &mut pieces,
                format!("[{label}]"),
                style.merged(TextStyle::colored(ColorRole::Accent).bold()),
            ),
            Node::SoftBreak => pieces.push(InlinePiece::SoftBreak),
            Node::HardBreak => pieces.push(InlinePiece::HardBreak),
            Node::Rule => push_inline_span(&mut pieces, "─", style),
            Node::TaskListMarker(checked) => {
                let (marker, marker_style) = if *checked {
                    ("✓ ", TextStyle::colored(ColorRole::Success).bold())
                } else {
                    ("○ ", TextStyle::colored(ColorRole::Muted))
                };
                push_inline_span(&mut pieces, marker, style.merged(marker_style));
            }
        }
    }
    pieces
}

fn apply_style(pieces: Vec<InlinePiece>, overlay: TextStyle) -> Vec<InlinePiece> {
    pieces
        .into_iter()
        .map(|piece| match piece {
            InlinePiece::Span(mut span) => {
                span.style = span.style.merged(overlay);
                InlinePiece::Span(span)
            }
            other => other,
        })
        .collect()
}

fn extend_inline(output: &mut Vec<InlinePiece>, pieces: Vec<InlinePiece>) {
    for piece in pieces {
        match piece {
            InlinePiece::Span(span) => push_inline_span(output, span.text, span.style),
            other => output.push(other),
        }
    }
}

fn push_inline_span(output: &mut Vec<InlinePiece>, text: impl AsRef<str>, style: TextStyle) {
    let text = text.as_ref();
    if text.is_empty() {
        return;
    }
    if let Some(InlinePiece::Span(last)) = output
        .last_mut()
        .filter(|piece| matches!(piece, InlinePiece::Span(last) if last.style == style))
    {
        last.text.push_str(text);
    } else {
        output.push(InlinePiece::Span(StyledSpan::new(text, style)));
    }
}

fn wrap_inline(pieces: &[InlinePiece], width: usize) -> Vec<StyledLine> {
    let width = width.max(1);
    let tokens = inline_tokens(pieces);
    let mut lines = Vec::new();
    let mut line = StyledLine::default();
    let mut pending_space = None;

    for token in tokens {
        match token {
            InlineToken::HardBreak => {
                trim_line_end(&mut line);
                lines.push(std::mem::take(&mut line));
                pending_space = None;
            }
            InlineToken::Space(style) => {
                if line.width() > 0 {
                    pending_space = Some(style);
                }
            }
            InlineToken::Word(mut units) => {
                let word_width = units.iter().map(|unit| unit.width).sum::<usize>();
                let separator_width = usize::from(pending_space.is_some() && line.width() > 0);
                if line.width() > 0 && line.width() + separator_width + word_width > width {
                    trim_line_end(&mut line);
                    lines.push(std::mem::take(&mut line));
                } else if let Some(style) = pending_space.take().filter(|_| line.width() > 0) {
                    line.push(" ", style);
                }
                pending_space = None;

                while !units.is_empty() {
                    let available = width.saturating_sub(line.width());
                    let mut take = 0;
                    let mut used = 0;
                    for unit in &units {
                        if used + unit.width > available && take > 0 {
                            break;
                        }
                        if unit.width > available && take == 0 {
                            break;
                        }
                        used += unit.width;
                        take += 1;
                    }
                    if take == 0 {
                        if line.width() > 0 {
                            lines.push(std::mem::take(&mut line));
                            continue;
                        }
                        let unit = units.remove(0);
                        line.push("…", unit.style);
                        lines.push(std::mem::take(&mut line));
                        continue;
                    }
                    for unit in units.drain(..take) {
                        line.push(unit.text, unit.style);
                    }
                    if !units.is_empty() {
                        lines.push(std::mem::take(&mut line));
                    }
                }
            }
        }
    }
    trim_line_end(&mut line);
    if !line.spans.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

fn inline_tokens(pieces: &[InlinePiece]) -> Vec<InlineToken> {
    let mut tokens = Vec::new();
    let mut word = Vec::new();
    let mut last_was_space = false;

    let flush_word = |tokens: &mut Vec<InlineToken>, word: &mut Vec<Unit>| {
        if !word.is_empty() {
            tokens.push(InlineToken::Word(std::mem::take(word)));
        }
    };

    for piece in pieces {
        match piece {
            InlinePiece::SoftBreak => {
                flush_word(&mut tokens, &mut word);
                if !last_was_space {
                    tokens.push(InlineToken::Space(TextStyle::default()));
                }
                last_was_space = true;
            }
            InlinePiece::HardBreak => {
                flush_word(&mut tokens, &mut word);
                tokens.push(InlineToken::HardBreak);
                last_was_space = false;
            }
            InlinePiece::Span(span) => {
                let preserve_spaces = span.style.color == ColorRole::Code;
                for grapheme in span.text.graphemes(true) {
                    if grapheme.contains('\n') || grapheme.contains('\r') {
                        flush_word(&mut tokens, &mut word);
                        tokens.push(InlineToken::HardBreak);
                        last_was_space = false;
                    } else if grapheme.chars().all(char::is_whitespace) && !preserve_spaces {
                        flush_word(&mut tokens, &mut word);
                        if !last_was_space {
                            tokens.push(InlineToken::Space(span.style));
                        }
                        last_was_space = true;
                    } else {
                        word.push(Unit {
                            text: grapheme.to_owned(),
                            style: span.style,
                            width: display_width(grapheme),
                        });
                        last_was_space = false;
                    }
                }
            }
        }
    }
    flush_word(&mut tokens, &mut word);
    tokens
}

struct LiteralLayout {
    lines: Vec<StyledLine>,
    line: StyledLine,
    column: usize,
    width: usize,
}

impl LiteralLayout {
    fn new(width: usize) -> Self {
        Self {
            lines: Vec::new(),
            line: StyledLine::default(),
            column: 0,
            width: width.max(1),
        }
    }

    fn push(&mut self, text: &str, style: TextStyle) {
        for grapheme in text.graphemes(true) {
            if grapheme.contains('\n') || grapheme.contains('\r') {
                self.lines.push(std::mem::take(&mut self.line));
                self.column = 0;
                continue;
            }
            let visible = if grapheme == "\t" {
                "    ".to_owned()
            } else {
                grapheme
                    .chars()
                    .filter(|character| !character.is_control())
                    .collect::<String>()
            };
            if visible.is_empty() {
                continue;
            }
            let visible_width = display_width(&visible);
            if self.column + visible_width > self.width && self.column > 0 {
                self.lines.push(std::mem::take(&mut self.line));
                self.column = 0;
            }
            if visible_width > self.width {
                self.line.push("…", style);
                self.lines.push(std::mem::take(&mut self.line));
                self.column = 0;
            } else {
                self.line.push(visible, style);
                self.column += visible_width;
            }
        }
    }

    fn finish(mut self, ends_with_line_break: bool) -> Vec<StyledLine> {
        if !self.line.spans.is_empty() || self.lines.is_empty() || !ends_with_line_break {
            self.lines.push(self.line);
        }
        self.lines
    }
}

fn highlight_code(content: &str, language: &str, width: usize) -> Option<Vec<StyledLine>> {
    let token = normalized_language_token(language);
    if token.is_empty() {
        return None;
    }

    let syntaxes = syntax_set();
    let syntax = syntaxes.find_syntax_by_token(&token)?;
    let mut parser = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut layout = LiteralLayout::new(width);

    for source_line in LinesWithEndings::from(content) {
        let operations = parser.parse_line(source_line, syntaxes).ok()?;
        for (region, operation) in ScopeRegionIterator::new(&operations, source_line) {
            stack.apply(operation).ok()?;
            if !region.is_empty() {
                layout.push(region, syntax_selectors().style(&stack));
            }
        }
    }
    Some(layout.finish(content.ends_with(['\n', '\r'])))
}

fn normalized_language_token(language: &str) -> String {
    let token = language
        .trim()
        .trim_matches(['{', '}', '.'])
        .split(',')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match token.as_str() {
        "c++" | "cplusplus" => "cpp",
        "csharp" => "cs",
        "golang" => "go",
        "javascript" => "js",
        "markdown" => "md",
        "python" => "py",
        "ruby" => "rb",
        "rust" => "rs",
        "shell" | "zsh" => "bash",
        "typescript" => "ts",
        "yaml" => "yml",
        _ => token.as_str(),
    }
    .to_owned()
}

fn wrap_preserving_whitespace(text: &str, width: usize, style: TextStyle) -> Vec<StyledLine> {
    let mut layout = LiteralLayout::new(width);
    layout.push(text, style);
    layout.finish(text.ends_with(['\n', '\r']))
}

fn natural_inline_width(pieces: &[InlinePiece]) -> usize {
    wrap_inline(pieces, usize::MAX / 4)
        .into_iter()
        .map(|line| line.width())
        .max()
        .unwrap_or(1)
}

fn align_line(mut line: StyledLine, width: usize, alignment: Alignment) -> StyledLine {
    let padding = width.saturating_sub(line.width());
    let (left, right) = match alignment {
        Alignment::Right => (padding, 0),
        Alignment::Center => (padding / 2, padding - padding / 2),
        Alignment::None | Alignment::Left => (0, padding),
    };
    line.prepend(" ".repeat(left), TextStyle::default());
    line.push(" ".repeat(right), TextStyle::default());
    line
}

fn pad_line_right(mut line: StyledLine, width: usize) -> StyledLine {
    line.push(
        " ".repeat(width.saturating_sub(line.width())),
        TextStyle::default(),
    );
    line
}

fn table_border(
    left: char,
    middle: char,
    right: char,
    widths: &[usize],
    style: TextStyle,
) -> StyledLine {
    let mut line = StyledLine::default();
    line.push(left.to_string(), style);
    for (index, width) in widths.iter().enumerate() {
        line.push("─".repeat(width.saturating_add(2)), style);
        line.push(
            if index + 1 == widths.len() {
                right
            } else {
                middle
            }
            .to_string(),
            style,
        );
    }
    line
}

fn single_box_border(left: char, right: char, width: usize, style: TextStyle) -> StyledLine {
    let mut line = StyledLine::default();
    line.push(left.to_string(), style);
    line.push("─".repeat(width.saturating_sub(2)), style);
    line.push(right.to_string(), style);
    line
}

fn collect_literal(nodes: &[Node], output: &mut String) {
    for node in nodes {
        match node {
            Node::Element { children, .. } => collect_literal(children, output),
            Node::Text(text)
            | Node::Code(text)
            | Node::InlineMath(text)
            | Node::DisplayMath(text)
            | Node::Html(text)
            | Node::InlineHtml(text) => output.push_str(text),
            Node::FootnoteReference(label) => {
                output.push_str("[^");
                output.push_str(label);
                output.push(']');
            }
            Node::SoftBreak | Node::HardBreak => output.push('\n'),
            Node::Rule => output.push_str("---"),
            Node::TaskListMarker(checked) => {
                output.push_str(if *checked { "[x] " } else { "[ ] " });
            }
        }
    }
}

fn inline_plain_text(nodes: &[Node]) -> String {
    let mut output = String::new();
    collect_literal(nodes, &mut output);
    output
}

fn append_block(output: &mut Vec<StyledLine>, mut block: Vec<StyledLine>) {
    trim_blank_lines(&mut block);
    if block.is_empty() {
        return;
    }
    if !output.is_empty() && !line_is_blank(output.last().expect("checked")) {
        output.push(StyledLine::default());
    }
    output.extend(block);
}

fn append_block_without_gap(output: &mut Vec<StyledLine>, mut block: Vec<StyledLine>) {
    trim_blank_lines(&mut block);
    output.extend(block);
}

fn trim_blank_lines(lines: &mut Vec<StyledLine>) {
    while lines.first().is_some_and(line_is_blank) {
        lines.remove(0);
    }
    while lines.last().is_some_and(line_is_blank) {
        lines.pop();
    }
}

fn line_is_blank(line: &StyledLine) -> bool {
    line.spans.iter().all(|span| span.text.trim().is_empty())
}

fn trim_line_end(line: &mut StyledLine) {
    while let Some(last) = line.spans.last_mut() {
        let trimmed = last.text.trim_end_matches(char::is_whitespace).to_owned();
        last.text = trimmed;
        if last.text.is_empty() {
            line.spans.pop();
        } else {
            break;
        }
    }
}

fn clip_line(line: StyledLine, width: usize) -> StyledLine {
    let mut output = StyledLine::default();
    let mut remaining = width;
    'spans: for span in line.spans {
        if remaining == 0 {
            break;
        }
        for grapheme in span.text.graphemes(true) {
            let grapheme_width = display_width(grapheme);
            if grapheme_width > remaining {
                break 'spans;
            }
            output.push(grapheme, span.style);
            remaining -= grapheme_width;
        }
    }
    output
}

fn truncate_plain(text: &str, width: usize) -> String {
    let mut output = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let grapheme_width = display_width(grapheme);
        if used + grapheme_width > width {
            break;
        }
        output.push_str(grapheme);
        used += grapheme_width;
    }
    output
}

fn is_html_break(html: &str) -> bool {
    let normalized = html.trim().to_ascii_lowercase();
    matches!(normalized.as_str(), "<br>" | "<br/>" | "<br />")
}

fn push_text_with_bare_urls(output: &mut Vec<InlinePiece>, text: &str, style: TextStyle) {
    let ranges = bare_url_ranges(text);
    if ranges.is_empty() {
        push_inline_span(output, text, style);
        return;
    }
    let mut cursor = 0;
    for range in ranges {
        push_inline_span(output, &text[cursor..range.start], style);
        push_inline_span(
            output,
            &text[range.clone()],
            style.merged(TextStyle {
                color: ColorRole::Link,
                underlined: true,
                ..TextStyle::default()
            }),
        );
        cursor = range.end;
    }
    push_inline_span(output, &text[cursor..], style);
}

fn bare_url_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut cursor = 0;
    while cursor < text.len() {
        let remaining = &text[cursor..];
        let Some(relative_start) = ["https://", "http://"]
            .into_iter()
            .filter_map(|prefix| remaining.find(prefix))
            .min()
        else {
            break;
        };
        let start = cursor + relative_start;
        if start > 0
            && text[..start]
                .chars()
                .next_back()
                .is_some_and(|character| character.is_alphanumeric() || character == '_')
        {
            cursor = start + 1;
            continue;
        }
        let candidate = &text[start..];
        let mut end = candidate.len();
        for (offset, character) in candidate.char_indices() {
            if offset > 0
                && (character.is_whitespace()
                    || character.is_control()
                    || matches!(character, '<' | '>' | '"' | '\'' | '`'))
            {
                end = offset;
                break;
            }
        }
        end = trim_bare_url_end(&candidate[..end]);
        let scheme_length = if candidate.starts_with("https://") {
            "https://".len()
        } else {
            "http://".len()
        };
        if end > scheme_length {
            ranges.push(start..start + end);
        }
        cursor = start + end.max(1);
    }
    ranges
}

fn trim_bare_url_end(candidate: &str) -> usize {
    let mut end = candidate.len();
    loop {
        let Some(character) = candidate[..end].chars().next_back() else {
            return end;
        };
        let trim = matches!(
            character,
            '.' | ',' | ';' | ':' | '!' | '?' | '*' | '。' | '，' | '；' | '：' | '！' | '？'
        ) || matches!(character, ')' | ']' | '}')
            && unmatched_closing_delimiter(&candidate[..end], character);
        if !trim {
            return end;
        }
        end -= character.len_utf8();
    }
}

fn unmatched_closing_delimiter(text: &str, closing: char) -> bool {
    let opening = match closing {
        ')' => '(',
        ']' => '[',
        '}' => '{',
        _ => return false,
    };
    text.chars()
        .filter(|character| *character == closing)
        .count()
        > text
            .chars()
            .filter(|character| *character == opening)
            .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(markdown: &str, width: usize) -> Vec<String> {
        render(markdown, width)
            .into_iter()
            .map(|line| line.plain_text())
            .collect()
    }

    fn assert_widths(markdown: &str, width: usize) {
        for line in render(markdown, width) {
            assert!(
                line.width() <= width,
                "width={} > {width}: {:?}",
                line.width(),
                line.plain_text()
            );
        }
    }

    fn text_with_color(lines: &[StyledLine], color: ColorRole) -> String {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| span.style.color == color)
            .map(|span| span.text.as_str())
            .collect()
    }

    #[test]
    fn component_has_the_requested_identity() {
        assert_eq!(COMPONENT_NAME, "agent-markdown-renderer");
    }

    #[test]
    fn renders_inline_semantics_as_structured_styles_without_ansi() {
        let markdown = concat!(
            "# Heading\n\n",
            "**bold** *italic* ~~deleted~~ `code` ",
            "[link](https://linked.example) https://bare.example/path?q=1.",
        );
        let lines = render(markdown, 120);
        let visible = lines
            .iter()
            .map(StyledLine::plain_text)
            .collect::<Vec<_>>()
            .join("\n");
        let spans = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .collect::<Vec<_>>();

        assert!(visible.contains("Heading"));
        assert!(visible.contains("bold italic deleted code"));
        assert!(
            ["**", "~~", "`", "# Heading"]
                .into_iter()
                .all(|marker| !visible.contains(marker))
        );
        assert!(
            spans
                .iter()
                .any(|span| span.text == "Heading" && span.style.bold && span.style.underlined)
        );
        assert!(
            spans
                .iter()
                .any(|span| span.text == "bold" && span.style.bold)
        );
        assert!(
            spans
                .iter()
                .any(|span| span.text == "italic" && span.style.italic)
        );
        assert!(
            spans
                .iter()
                .any(|span| span.text == "deleted" && span.style.crossed_out)
        );
        assert!(
            spans
                .iter()
                .any(|span| span.text == "code" && span.style.color == ColorRole::Code)
        );
        assert!(spans.iter().any(|span| span.text == "link"
            && span.style.color == ColorRole::Link
            && span.style.underlined));
        assert!(
            spans
                .iter()
                .any(|span| span.text == "https://bare.example/path?q=1"
                    && span.style.color == ColorRole::Link
                    && span.style.underlined)
        );
        assert!(
            spans
                .iter()
                .all(|span| !span.text.contains('\u{1b}') && !span.text.contains('\0'))
        );
    }

    #[test]
    fn renders_nested_styles_and_reflows_them_across_lines() {
        let lines = render("**before [linked words](https://example.com) after**", 12);
        assert!(lines.len() >= 3);
        assert!(lines.iter().all(|line| line.width() <= 12));
        let linked = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| span.style.color == ColorRole::Link)
            .collect::<Vec<_>>();
        assert!(!linked.is_empty());
        assert!(
            linked
                .iter()
                .all(|span| span.style.bold && span.style.underlined)
        );
    }

    #[test]
    fn renders_cjk_adjacent_strong_without_exposing_compatibility_markers() {
        let markdown = concat!(
            "一句话：**别生吃、别半生不熟，炖熟煮透就完全可以放心吃。**\n\n",
            "豆橛子是对**芸豆（豆角、四季豆类）**的俗称。\n\n",
            "含有**植物血凝素（PHA，一种凝集素）**和**皂苷**等天然毒素。\n\n",
            "这是**（非常重要）**的提醒。\n\n",
            "`代码**（保持原样）**文本` \\**（转义保持原样）**文本\n",
        );
        let lines = render(markdown, 200);
        let visible = lines
            .iter()
            .map(StyledLine::plain_text)
            .collect::<Vec<_>>()
            .join("\n");
        let bold = lines
            .iter()
            .flat_map(|line| &line.spans)
            .filter(|span| span.style.bold)
            .map(|span| span.text.as_str())
            .collect::<String>();

        assert!(bold.contains("别生吃、别半生不熟，炖熟煮透就完全可以放心吃。"));
        assert!(bold.contains("芸豆（豆角、四季豆类）"));
        assert!(bold.contains("植物血凝素（PHA，一种凝集素）"));
        assert!(bold.contains("皂苷"));
        assert!(bold.contains("（非常重要）"));
        assert!(visible.contains("代码**（保持原样）**文本"));
        assert!(visible.contains("**（转义保持原样）**文本"));
        assert!(!visible.contains('\u{000b}'));
    }

    #[test]
    fn renders_lists_tasks_quotes_alerts_and_definition_lists() {
        let markdown = concat!(
            "> [!WARNING]\n",
            "> quoted **text**\n\n",
            "3. first\n",
            "4. second\n",
            "   - [x] nested done\n",
            "   - [ ] nested pending\n\n",
            "Term\n",
            ": definition\n",
        );
        let lines = render(markdown, 48);
        let visible = lines
            .iter()
            .map(StyledLine::plain_text)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(visible.contains("│ WARNING"));
        assert!(visible.contains("│ quoted text"));
        assert!(visible.contains("3. first"));
        assert!(visible.contains("4. second"));
        assert!(visible.contains("• ✓ nested done"));
        assert!(visible.contains("• ○ nested pending"));
        assert!(visible.contains("Term"));
        assert!(visible.contains("  · definition"));
        assert!(lines.iter().flat_map(|line| &line.spans).any(|span| {
            span.text == "✓ " && span.style.color == ColorRole::Success && span.style.bold
        }));
        assert!(lines.iter().flat_map(|line| &line.spans).any(|span| {
            span.text.contains('○') && span.style.color == ColorRole::Muted && !span.style.bold
        }));
        assert_widths(markdown, 48);
    }

    #[test]
    fn renders_code_blocks_with_default_background_open_sides_and_language() {
        let markdown = "```rust linenos\nfn main() {\n\tprintln!(\"你好 👩🏽‍💻\");\n}\n```";
        let lines = render(markdown, 32);
        let visible = lines.iter().map(StyledLine::plain_text).collect::<Vec<_>>();

        assert!(
            visible
                .first()
                .is_some_and(|line| line.starts_with("╭─ rust") && line.ends_with('╮'))
        );
        assert!(
            visible
                .last()
                .is_some_and(|line| line == &format!("╰{}╯", "─".repeat(30)))
        );
        assert!(visible.iter().any(|line| line.contains("fn main()")));
        assert!(visible.iter().any(|line| line.contains("println!")));
        assert!(visible.iter().any(|line| line == "  fn main() {"));
        assert!(
            visible
                .iter()
                .any(|line| line.starts_with("      println!"))
        );
        assert!(
            visible[1..visible.len() - 1]
                .iter()
                .all(|line| !line.starts_with('│') && !line.ends_with('│'))
        );
        assert!(
            visible[1..visible.len() - 1]
                .iter()
                .all(|line| line.starts_with("  ") && display_width(line) <= 30)
        );
        assert!(text_with_color(&lines, ColorRole::SyntaxDeclaration).contains("fn"));
        assert!(text_with_color(&lines, ColorRole::SyntaxFunction).contains("main"));
        assert!(text_with_color(&lines, ColorRole::SyntaxString).contains("你好"));
        assert!(lines.iter().all(|line| line.width() <= 32));
    }

    #[test]
    fn highlights_known_languages_and_safely_falls_back_for_unknown_fences() {
        let rust = render(
            "```rust\nfn answer(value: i32) -> i32 {\n    /* first\n       second */\n    let message = \"ok\";\n    value + 42\n}\n```",
            80,
        );
        assert!(text_with_color(&rust, ColorRole::SyntaxDeclaration).contains("fn"));
        assert!(text_with_color(&rust, ColorRole::SyntaxFunction).contains("answer"));
        assert!(text_with_color(&rust, ColorRole::SyntaxComment).contains("first"));
        assert!(text_with_color(&rust, ColorRole::SyntaxComment).contains("second"));
        assert!(text_with_color(&rust, ColorRole::SyntaxString).contains("\"ok\""));
        assert!(text_with_color(&rust, ColorRole::SyntaxNumber).contains("42"));

        let python = render(
            "```python\ndef greet(name):\n    # comment\n    return f\"hello {name}\"\n```",
            80,
        );
        assert!(text_with_color(&python, ColorRole::SyntaxFunction).contains("greet"));
        assert!(text_with_color(&python, ColorRole::SyntaxComment).contains("comment"));
        assert!(text_with_color(&python, ColorRole::SyntaxString).contains("hello"));
        assert!(text_with_color(&python, ColorRole::SyntaxKeyword).contains("return"));

        let shell = render(
            "```shell\nif test -n \"$HOME\"; then\n    echo \"ready\"\nfi\n```",
            80,
        );
        assert!(text_with_color(&shell, ColorRole::SyntaxKeyword).contains("if"));
        assert!(text_with_color(&shell, ColorRole::SyntaxString).contains("ready"));

        let unknown = render("```made-up-language\nlet value = \"unchanged\";\n```", 80);
        assert!(
            text_with_color(&unknown, ColorRole::Primary).contains("let value = \"unchanged\";")
        );
        assert!(
            unknown
                .iter()
                .any(|line| line.plain_text() == "  let value = \"unchanged\";")
        );

        let unlabeled = render("```\nplain code\n```", 80);
        assert!(text_with_color(&unlabeled, ColorRole::Primary).contains("plain code"));
        assert!(!text_with_color(&unlabeled, ColorRole::Code).contains("plain code"));
    }

    #[test]
    fn recognizes_common_markdown_fence_language_names_and_aliases() {
        for language in [
            "rust",
            "rs",
            "python",
            "javascript",
            "typescript",
            "bash",
            "shell",
            "c",
            "c++",
            "cpp",
            "csharp",
            "java",
            "go",
            "json",
            "yaml",
            "html",
            "css",
            "sql",
            "toml",
            "dockerfile",
        ] {
            let token = normalized_language_token(language);
            assert!(
                syntax_set().find_syntax_by_token(&token).is_some(),
                "language fence `{language}` normalized to `{token}` was not recognized"
            );
        }
        assert_eq!(normalized_language_token("{.rust,ignore}"), "rs");
        assert!(
            syntax_set()
                .find_syntax_by_token("made-up-language")
                .is_none()
        );
    }

    #[test]
    fn syntax_highlighting_preserves_text_styles_and_width_across_wrapping() {
        let markdown = "```rs\nfn extremely_long_function_name() { println!(\"你好👩🏽‍💻\"); }\n```";
        let lines = render(markdown, 18);
        let visible = lines.iter().map(StyledLine::plain_text).collect::<Vec<_>>();
        let rejoined_code = visible[1..visible.len() - 1]
            .iter()
            .map(|line| line.strip_prefix("  ").unwrap_or(line))
            .collect::<String>();

        assert!(rejoined_code.contains("fn extremely_long_function_name()"));
        assert!(text_with_color(&lines, ColorRole::SyntaxFunction).contains("extremely_long"));
        assert!(text_with_color(&lines, ColorRole::SyntaxString).contains("你好"));
        assert!(lines.iter().all(|line| line.width() <= 18));
    }

    #[test]
    fn renders_full_border_tables_with_alignment_unicode_and_wrapping() {
        let markdown = concat!(
            "| Left | Center | Right |\n",
            "| :--- | :----: | ----: |\n",
            "| 中文 | 👩🏽‍💻 | a long value that wraps |\n",
            "| x | y | 42 |\n",
        );
        let lines = render(markdown, 44);
        let visible = lines.iter().map(StyledLine::plain_text).collect::<Vec<_>>();

        assert!(visible.first().is_some_and(|line| line.starts_with('┌')));
        assert!(visible.first().is_some_and(|line| line.ends_with('┐')));
        assert!(
            visible
                .iter()
                .any(|line| line.starts_with('├') && line.ends_with('┤'))
        );
        assert!(visible.last().is_some_and(|line| line.starts_with('└')));
        assert!(visible.last().is_some_and(|line| line.ends_with('┘')));
        assert!(visible.iter().any(|line| line.contains("中文")));
        assert!(visible.iter().any(|line| line.contains("👩🏽‍💻")));
        let table_width = visible.first().map(|line| display_width(line)).unwrap();
        assert!(table_width <= 44);
        assert!(
            visible
                .iter()
                .all(|line| display_width(line) == table_width)
        );

        let compact = plain("| A | B |\n|---|---|\n| 1 | 2 |", 80);
        let compact_width = display_width(&compact[0]);
        assert!(compact_width < 80);
        assert!(
            compact
                .iter()
                .all(|line| display_width(line) == compact_width)
        );
    }

    #[test]
    fn falls_back_to_a_framed_stacked_table_on_narrow_terminals() {
        let markdown =
            "| Name | Value |\n|---|---|\n| language | Rust |\n| status | stable and readable |";
        let lines = render(markdown, 24);
        let visible = lines.iter().map(StyledLine::plain_text).collect::<Vec<_>>();

        assert!(
            visible
                .first()
                .is_some_and(|line| line == "┌──────────────────────┐")
        );
        assert!(visible.iter().any(|line| line.contains("Name: language")));
        assert!(visible.iter().any(|line| line.contains("Value: Rust")));
        assert!(
            visible
                .last()
                .is_some_and(|line| line == "└──────────────────────┘")
        );
        assert!(visible.iter().all(|line| display_width(line) == 24));
    }

    #[test]
    fn renders_math_footnotes_images_and_safe_html_fallbacks() {
        let markdown = concat!(
            "Inline $E=mc^2$, image ![diagram](diagram.png), note[^n].\n\n",
            "$$\\sum_{i=1}^{n} i$$\n\n",
            "<span>inline html</span>\n\n",
            "<div>\nraw block\n</div>\n\n",
            "[^n]: footnote body\n",
        );
        let lines = render(markdown, 50);
        let visible = lines
            .iter()
            .map(StyledLine::plain_text)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(visible.contains("Inline E=mc^2"));
        assert!(visible.contains("▧ diagram"));
        assert!(visible.contains("[n]"));
        assert!(visible.contains("│ \\sum_{i=1}^{n} i"));
        assert!(visible.contains("inline html"));
        assert!(visible.contains("<div>"));
        assert!(visible.contains("[n] footnote body"));
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.style.color == ColorRole::Math && span.text.contains("E=mc"))
        );
    }

    #[test]
    fn is_unicode_width_correct_and_never_splits_grapheme_clusters() {
        assert_eq!(display_width("👩🏽‍💻"), 2);
        assert_eq!(display_width("e\u{301}"), 1);
        assert_eq!(display_width("中文"), 4);

        let markdown = "**👩🏽‍💻e\u{301}中文abcdef**";
        for width in 1..=12 {
            let lines = render(markdown, width);
            assert!(lines.iter().all(|line| line.width() <= width));
            assert!(
                lines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .all(|span| !span.text.starts_with('\u{200d}'))
            );
        }
    }

    #[test]
    fn every_streaming_prefix_and_pathological_input_is_deterministic_and_safe() {
        let markdown = concat!(
            "# 流式 👩🏽‍💻\n\n",
            "> [!NOTE]\n> **bold _nested_** and $x^2$\n\n",
            "| A | B |\n|:-|:-:|\n| `a\\|b` | [链接](https://例子.测试) |\n\n",
            "```rust\nfn main() { println!(\"你好\"); }\n```\n",
        );
        for end in markdown
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(markdown.len()))
        {
            let prefix = &markdown[..end];
            for width in [1, 2, 3, 4, 8, 20, 80] {
                let first = render(prefix, width);
                assert_eq!(first, render(prefix, width));
                assert!(first.iter().all(|line| line.width() <= width));
            }
        }

        let storm = format!(
            "{}middle{}",
            "*_~^`[$".repeat(2_000),
            "$]`^~_*".repeat(2_000)
        );
        assert_widths(&storm, 37);
    }

    #[test]
    fn removes_terminal_controls_without_mutating_visible_plain_text() {
        let markdown = "before\u{1b}[2Jafter\0 **safe**\u{7}";
        let lines = render(markdown, 80);
        let visible = lines
            .iter()
            .map(StyledLine::plain_text)
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(visible, "before[2Jafter safe");
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| span.text.chars().all(|character| !character.is_control()))
        );
    }
}
