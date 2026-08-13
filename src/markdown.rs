use std::borrow::Cow;

use pulldown_cmark::{Options, Parser};

const CJK_EMPHASIS_SENTINEL: char = '\u{000b}';

#[derive(Clone, Copy)]
struct StrongMarker {
    offset: usize,
    standard_open: bool,
    standard_close: bool,
    compatibility_open: bool,
    compatibility_close: bool,
    insert_before: bool,
    insert_after: bool,
}

#[derive(Clone, Copy)]
struct CodeDelimiter {
    byte: u8,
    length: usize,
    fenced: bool,
}

pub fn parser(markdown: &str) -> Parser<'_> {
    Parser::new_ext(markdown, options())
}

pub fn normalize_cjk_emphasis(markdown: &str) -> Cow<'_, str> {
    let mut markers = collect_strong_markers(markdown);
    let mut open = Vec::new();
    let mut pairs = Vec::new();
    for (index, marker) in markers.iter().enumerate() {
        if !open.is_empty() && (marker.standard_close || marker.compatibility_close) {
            pairs.push((open.pop().unwrap(), index));
        } else if marker.standard_open || marker.compatibility_open {
            open.push(index);
        }
    }
    for (opening, closing) in pairs {
        if !markers[opening].standard_open && markers[opening].compatibility_open {
            markers[opening].insert_before = true;
        }
        if !markers[closing].standard_close && markers[closing].compatibility_close {
            markers[closing].insert_after = true;
        }
    }
    if !markers
        .iter()
        .any(|marker| marker.insert_before || marker.insert_after)
    {
        return Cow::Borrowed(markdown);
    }

    let mut output = String::with_capacity(markdown.len() + markers.len());
    let mut cursor = 0;
    for marker in markers {
        output.push_str(&markdown[cursor..marker.offset]);
        if marker.insert_before {
            output.push(CJK_EMPHASIS_SENTINEL);
        }
        output.push_str("**");
        if marker.insert_after {
            output.push(CJK_EMPHASIS_SENTINEL);
        }
        cursor = marker.offset + 2;
    }
    output.push_str(&markdown[cursor..]);
    Cow::Owned(output)
}

fn collect_strong_markers(markdown: &str) -> Vec<StrongMarker> {
    let bytes = markdown.as_bytes();
    let mut markers = Vec::new();
    let mut code = None::<CodeDelimiter>;
    let mut cursor = 0;
    while cursor < markdown.len() {
        let byte = bytes[cursor];
        if byte == b'`' || byte == b'~' {
            let length = bytes[cursor..]
                .iter()
                .take_while(|candidate| **candidate == byte)
                .count();
            let fence_position = is_fence_position(markdown, cursor);
            if let Some(delimiter) = code {
                let closes = delimiter.byte == byte
                    && if delimiter.fenced {
                        fence_position && length >= delimiter.length
                    } else {
                        length == delimiter.length
                    };
                if closes {
                    code = None;
                }
                cursor += length;
                continue;
            }
            if byte == b'`' || (byte == b'~' && fence_position && length >= 3) {
                code = Some(CodeDelimiter {
                    byte,
                    length,
                    fenced: fence_position && length >= 3,
                });
                cursor += length;
                continue;
            }
        }
        if code.is_some() {
            cursor += markdown[cursor..].chars().next().unwrap().len_utf8();
            continue;
        }
        if bytes[cursor..].starts_with(b"**")
            && (cursor == 0 || bytes[cursor - 1] != b'*')
            && (cursor + 2 == bytes.len() || bytes[cursor + 2] != b'*')
            && !is_escaped(markdown, cursor)
            && !inside_link_target(markdown, cursor)
        {
            let previous = markdown[..cursor].chars().next_back();
            let next = markdown[cursor + 2..].chars().next();
            let previous_whitespace = previous.is_none_or(char::is_whitespace);
            let next_whitespace = next.is_none_or(char::is_whitespace);
            let previous_punctuation = previous.is_some_and(is_punctuation_like);
            let next_punctuation = next.is_some_and(is_punctuation_like);
            let standard_open = !next_whitespace
                && (!next_punctuation || previous_whitespace || previous_punctuation);
            let standard_close = !previous_whitespace
                && (!previous_punctuation || next_whitespace || next_punctuation);
            let compatibility_open = previous.is_some_and(is_cjk) && next_punctuation;
            let compatibility_close = previous_punctuation && next.is_some_and(is_cjk);
            if standard_open || standard_close || compatibility_open || compatibility_close {
                markers.push(StrongMarker {
                    offset: cursor,
                    standard_open,
                    standard_close,
                    compatibility_open,
                    compatibility_close,
                    insert_before: false,
                    insert_after: false,
                });
            }
            cursor += 2;
            continue;
        }
        cursor += markdown[cursor..].chars().next().unwrap().len_utf8();
    }
    markers
}

fn is_escaped(markdown: &str, index: usize) -> bool {
    markdown.as_bytes()[..index]
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count()
        % 2
        == 1
}

fn is_fence_position(markdown: &str, index: usize) -> bool {
    let line_start = markdown[..index].rfind('\n').map_or(0, |offset| offset + 1);
    let prefix = &markdown.as_bytes()[line_start..index];
    prefix.len() <= 3 && prefix.iter().all(|byte| *byte == b' ')
}

fn inside_link_target(markdown: &str, index: usize) -> bool {
    let line = &markdown[markdown[..index].rfind('\n').map_or(0, |offset| offset + 1)..index];
    let token = line
        .rsplit_once(char::is_whitespace)
        .map_or(line, |(_, token)| token);
    if token.contains("://") {
        return true;
    }
    line.rfind("](")
        .is_some_and(|open| line.rfind(')').is_none_or(|close| open > close))
}

fn is_punctuation_like(character: char) -> bool {
    !character.is_alphanumeric() && !character.is_whitespace()
}

fn is_cjk(character: char) -> bool {
    matches!(
        character,
        '\u{1100}'..='\u{11ff}'
            | '\u{3040}'..='\u{30ff}'
            | '\u{3100}'..='\u{312f}'
            | '\u{3400}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{ac00}'..='\u{d7af}'
            | '\u{f900}'..='\u{faff}'
            | '\u{20000}'..='\u{2fa1f}'
    )
}

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM
        | Options::ENABLE_DEFINITION_LIST
        | Options::ENABLE_SUPERSCRIPT
        | Options::ENABLE_SUBSCRIPT
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use pulldown_cmark::{
        Alignment, BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, LinkType, Options, Tag,
        TagEnd,
    };

    use super::{CJK_EMPHASIS_SENTINEL, normalize_cjk_emphasis, options, parser};

    fn events(markdown: &str) -> Vec<Event<'_>> {
        parser(markdown).collect()
    }

    fn text(events: &[Event<'_>]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Text(text) => Some(text.as_ref()),
                _ => None,
            })
            .collect()
    }

    fn assert_well_formed(markdown: &str) {
        let mut open_tags = Vec::new();
        for (event, range) in parser(markdown).into_offset_iter() {
            assert!(range.start <= range.end, "{event:?}: {range:?}");
            assert!(range.end <= markdown.len(), "{event:?}: {range:?}");
            assert!(
                markdown.is_char_boundary(range.start),
                "{event:?}: {range:?}"
            );
            assert!(markdown.is_char_boundary(range.end), "{event:?}: {range:?}");

            match event {
                Event::Start(tag) => open_tags.push(tag.to_end()),
                Event::End(actual) => {
                    assert_eq!(open_tags.pop(), Some(actual), "{markdown:?}");
                }
                _ => {}
            }
        }
        assert!(open_tags.is_empty(), "{markdown:?}");
        assert_eq!(
            parser(markdown).collect::<Vec<_>>(),
            parser(markdown).collect::<Vec<_>>(),
            "{markdown:?}"
        );
    }

    fn count_start(events: &[Event<'_>], expected: TagEnd) -> usize {
        events
            .iter()
            .filter(|event| matches!(event, Event::Start(tag) if tag.to_end() == expected))
            .count()
    }

    fn event_kind(event: &Event<'_>) -> &'static str {
        match event {
            Event::Start(_) => "start",
            Event::End(_) => "end",
            Event::Text(_) => "text",
            Event::Code(_) => "code",
            Event::InlineMath(_) => "inline_math",
            Event::DisplayMath(_) => "display_math",
            Event::Html(_) => "html",
            Event::InlineHtml(_) => "inline_html",
            Event::FootnoteReference(_) => "footnote_reference",
            Event::SoftBreak => "soft_break",
            Event::HardBreak => "hard_break",
            Event::Rule => "rule",
            Event::TaskListMarker(_) => "task_list_marker",
        }
    }

    #[test]
    fn parses_commonmark_and_structural_extensions() {
        let markdown = concat!(
            "# Heading\n\n",
            "**bold** *emphasis* ~~deleted~~ `code`\n\n",
            "> quote\n\n",
            "1. ordered\n",
            "2. second\n\n",
            "- [x] task\n\n",
            "```rust\nfn main() {}\n```\n\n",
            "| A | B |\n|---|---|\n| 1 | 2 |\n\n",
            "[link](https://example.com) ![image](image.png)\n",
        );
        let events = parser(markdown).collect::<Vec<_>>();

        assert!(events.iter().any(|event| matches!(
            event,
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            })
        )));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Start(Tag::Strong)))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Start(Tag::Emphasis)))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Start(Tag::Strikethrough)))
        );
        assert!(events.iter().any(|event| matches!(event, Event::Code(_))));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Start(Tag::BlockQuote(_))))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Start(Tag::List(Some(1)))))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::TaskListMarker(true)))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Start(Tag::CodeBlock(_))))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Start(Tag::Table(_))))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Start(Tag::Link { .. })))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Start(Tag::Image { .. })))
        );
    }

    #[test]
    fn parses_block_structure_breaks_and_nested_containers() {
        let markdown = concat!(
            "# ATX\n\n",
            "Setext\n------\n\n",
            "***\n\n",
            "> outer\n",
            "> > inner\n\n",
            "- unordered\n",
            "\n",
            "  3. ordered from three\n\n",
            "soft\nbreak  \n",
            "hard one\\\n",
            "hard two\n",
        );
        let parsed = events(markdown);

        assert!(parsed.iter().any(|event| matches!(
            event,
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            })
        )));
        assert!(parsed.iter().any(|event| matches!(
            event,
            Event::Start(Tag::Heading {
                level: HeadingLevel::H2,
                ..
            })
        )));
        assert!(parsed.iter().any(|event| matches!(event, Event::Rule)));
        assert_eq!(count_start(&parsed, TagEnd::BlockQuote(None)), 2);
        assert!(
            parsed
                .iter()
                .any(|event| matches!(event, Event::Start(Tag::List(None))))
        );
        assert!(
            parsed
                .iter()
                .any(|event| matches!(event, Event::Start(Tag::List(Some(3)))))
        );
        assert_eq!(
            parsed
                .iter()
                .filter(|event| matches!(event, Event::SoftBreak))
                .count(),
            1
        );
        assert_eq!(
            parsed
                .iter()
                .filter(|event| matches!(event, Event::HardBreak))
                .count(),
            2
        );
        assert_well_formed(markdown);
    }

    #[test]
    fn distinguishes_fenced_indented_inline_code_and_literal_markers() {
        let markdown = concat!(
            "````rust linenos\n",
            "``` is literal inside a longer fence\n",
            "**not strong** $not math$\n",
            "````\n\n",
            "    indented <b>code</b>\n\n",
            "``code with ` backtick`` and ` **literal** [link](x) $math$ `\n",
        );
        let parsed = events(markdown);

        assert!(parsed.iter().any(|event| matches!(
            event,
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
                if info.as_ref() == "rust linenos"
        )));
        assert!(
            parsed.iter().any(|event| matches!(
                event,
                Event::Start(Tag::CodeBlock(CodeBlockKind::Indented))
            ))
        );
        assert!(parsed.iter().any(
            |event| matches!(event, Event::Code(code) if code.as_ref() == "code with ` backtick")
        ));
        assert!(parsed.iter().any(
            |event| matches!(event, Event::Code(code) if code.as_ref() == "**literal** [link](x) $math$")
        ));
        assert_eq!(count_start(&parsed, TagEnd::Strong), 0);
        assert_eq!(count_start(&parsed, TagEnd::Link), 0);
        assert!(
            !parsed
                .iter()
                .any(|event| matches!(event, Event::InlineMath(_) | Event::DisplayMath(_)))
        );
        assert_well_formed(markdown);
    }

    #[test]
    fn parses_table_alignment_escaped_pipes_and_inline_markup() {
        let markdown = concat!(
            "| left | center | right | plain |\n",
            "| :--- | :----: | ----: | ----- |\n",
            "| a\\|b | `c\\|d` | **bold** | 中文 👩🏽‍💻 |\n",
        );
        let parsed = events(markdown);

        assert!(parsed.iter().any(|event| matches!(
            event,
            Event::Start(Tag::Table(alignments))
                if alignments == &[Alignment::Left, Alignment::Center, Alignment::Right, Alignment::None]
        )));
        assert_eq!(count_start(&parsed, TagEnd::TableCell), 8);
        assert!(text(&parsed).contains("a|b"));
        assert!(
            parsed
                .iter()
                .any(|event| matches!(event, Event::Code(value) if value.as_ref() == "c|d"))
        );
        assert_eq!(count_start(&parsed, TagEnd::Strong), 1);
        assert_well_formed(markdown);
    }

    #[test]
    fn requires_pipes_inside_table_code_spans_to_be_escaped() {
        let markdown = "| A | B |\n|---|---|\n| `x|y` | z |";
        let parsed = events(markdown);

        assert!(!parsed.iter().any(|event| matches!(event, Event::Code(_))));
        assert!(text(&parsed).contains('`'));
        assert_well_formed(markdown);
    }

    #[test]
    fn parses_all_common_link_forms_images_and_titles() {
        let markdown = concat!(
            "[inline](https://example.com/a_(b) \"inline title\") ",
            "[reference][id] [collapsed][] [shortcut] ",
            "<https://auto.example/a?q=1> <me@example.com> ",
            "![alt *emphasis*](image.png \"image title\")\n\n",
            "[id]: /reference \"reference title\"\n",
            "[collapsed]: /collapsed\n",
            "[shortcut]: /shortcut\n",
        );
        let parsed = events(markdown);
        let links = parsed
            .iter()
            .filter_map(|event| match event {
                Event::Start(Tag::Link {
                    link_type,
                    dest_url,
                    title,
                    ..
                }) => Some((*link_type, dest_url.as_ref(), title.as_ref())),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(links.contains(&(
            LinkType::Inline,
            "https://example.com/a_(b)",
            "inline title"
        )));
        assert!(links.contains(&(LinkType::Reference, "/reference", "reference title")));
        assert!(links.contains(&(LinkType::Collapsed, "/collapsed", "")));
        assert!(links.contains(&(LinkType::Shortcut, "/shortcut", "")));
        assert!(links.contains(&(LinkType::Autolink, "https://auto.example/a?q=1", "")));
        assert!(links.contains(&(LinkType::Email, "me@example.com", "")));
        assert!(parsed.iter().any(|event| matches!(
            event,
            Event::Start(Tag::Image {
                link_type: LinkType::Inline,
                dest_url,
                title,
                ..
            }) if dest_url.as_ref() == "image.png" && title.as_ref() == "image title"
        )));
        assert_eq!(count_start(&parsed, TagEnd::Emphasis), 1);
        assert_well_formed(markdown);
    }

    #[test]
    fn reference_labels_are_case_insensitive_and_first_definition_wins() {
        let markdown = "[value][Case]\n\n[case]: /first\n[CASE]: /second";
        let parsed = events(markdown);

        assert!(parsed.iter().any(|event| matches!(
            event,
            Event::Start(Tag::Link {
                link_type: LinkType::Reference,
                dest_url,
                ..
            }) if dest_url.as_ref() == "/first"
        )));
        assert!(!parsed.iter().any(|event| matches!(
            event,
            Event::Start(Tag::Link { dest_url, .. }) if dest_url.as_ref() == "/second"
        )));
        assert_well_formed(markdown);
    }

    #[test]
    fn leaves_bare_urls_and_unresolved_references_as_literal_text() {
        let markdown =
            "https://bare.example/path?q=1 [missing] [also missing][unknown] <not a tag>";
        let parsed = events(markdown);

        assert_eq!(count_start(&parsed, TagEnd::Link), 0);
        let literal = text(&parsed);
        assert!(literal.contains("https://bare.example/path?q=1"));
        assert!(literal.contains("[missing]"));
        assert!(literal.contains("[also missing][unknown]"));
        assert_well_formed(markdown);
    }

    #[test]
    fn prevents_nested_links_but_keeps_the_inner_valid_link() {
        let markdown = "[outer [inner](https://inner.example)](https://outer.example)";
        let parsed = events(markdown);
        let mut link_depth = 0;
        let mut maximum_link_depth = 0;
        for event in &parsed {
            match event {
                Event::Start(Tag::Link { .. }) => {
                    link_depth += 1;
                    maximum_link_depth = maximum_link_depth.max(link_depth);
                }
                Event::End(TagEnd::Link) => link_depth -= 1,
                _ => {}
            }
        }

        assert_eq!(maximum_link_depth, 1);
        assert_eq!(link_depth, 0);
        assert_well_formed(markdown);
    }

    #[test]
    fn decodes_entities_and_escapes_without_touching_raw_html() {
        let markdown = concat!(
            r"\*literal\* \[brackets\] &amp; &#x4E2D; ",
            "<span data-x=\"1\">inline</span>\n\n",
            "<div>\n<strong>block</strong>\n</div>\n",
        );
        let parsed = events(markdown);
        let literal = text(&parsed);

        assert!(literal.contains("*literal* [brackets] & 中 "));
        assert!(parsed.iter().any(
            |event| matches!(event, Event::InlineHtml(html) if html.as_ref() == "<span data-x=\"1\">")
        ));
        assert!(
            parsed.iter().any(
                |event| matches!(event, Event::InlineHtml(html) if html.as_ref() == "</span>")
            )
        );
        assert!(
            parsed
                .iter()
                .any(|event| matches!(event, Event::Start(Tag::HtmlBlock)))
        );
        assert!(parsed.iter().any(
            |event| matches!(event, Event::Html(html) if html.contains("<strong>block</strong>"))
        ));
        assert_well_formed(markdown);
    }

    #[test]
    fn resolves_delimiter_ambiguity_and_enabled_inline_extensions() {
        let markdown = "***both*** foo_bar_baz a***b**c* ~~deleted~~ ~subscript~ ^superscript^";
        let parsed = events(markdown);

        assert_eq!(count_start(&parsed, TagEnd::Strong), 2);
        assert_eq!(count_start(&parsed, TagEnd::Emphasis), 2);
        assert_eq!(count_start(&parsed, TagEnd::Strikethrough), 1);
        assert_eq!(count_start(&parsed, TagEnd::Subscript), 1);
        assert_eq!(count_start(&parsed, TagEnd::Superscript), 1);
        assert!(text(&parsed).contains("foo_bar_baz"));
        assert_well_formed(markdown);
    }

    #[test]
    fn cjk_strong_compatibility_is_targeted_and_preserves_code() {
        let markdown = concat!(
            "一句话：**别生吃、别半生不熟，炖熟煮透就完全可以放心吃。**\n",
            "豆橛子是对**芸豆（豆角、四季豆类）**的俗称。\n",
            "含有**植物血凝素（PHA，一种凝集素）**和**皂苷**等天然毒素。\n",
            "这是**（非常重要）**的提醒。\n",
            "`代码**（保持原样）**文本` \\**（转义保持原样）**文本\n\n",
            "[链接](https://例子.测试/**（路径）**页面)\n\n",
            "```text\n围栏**（保持原样）**文本\n```\n",
            "未闭合**（保持字面量）\n",
        );
        let normalized = normalize_cjk_emphasis(markdown);
        let parsed = parser(normalized.as_ref()).collect::<Vec<_>>();
        let restored = normalized.replace(CJK_EMPHASIS_SENTINEL, "");

        assert_eq!(count_start(&parsed, TagEnd::Strong), 5);
        assert!(normalized.contains(CJK_EMPHASIS_SENTINEL));
        assert!(restored.contains("一句话：**别生吃、别半生不熟，炖熟煮透就完全可以放心吃。**"));
        assert!(restored.contains("`代码**（保持原样）**文本`"));
        assert!(restored.contains("围栏**（保持原样）**文本"));
        assert!(restored.contains("\\**（转义保持原样）**文本"));
        assert!(restored.contains("https://例子.测试/**（路径）**页面"));
        assert!(restored.contains("未闭合**（保持字面量）"));
    }

    #[test]
    fn parses_footnotes_definition_lists_and_task_states() {
        let markdown = concat!(
            "Term\n",
            ": first definition\n",
            ": second definition\n\n",
            "Reference before definition[^note].\n\n",
            "[^note]: Footnote with **strong text**.\n\n",
            "- [ ] pending\n",
            "- [x] done lowercase\n",
            "- [X] done uppercase\n",
        );
        let parsed = events(markdown);

        assert_eq!(count_start(&parsed, TagEnd::DefinitionList), 1);
        assert_eq!(count_start(&parsed, TagEnd::DefinitionListTitle), 1);
        assert_eq!(count_start(&parsed, TagEnd::DefinitionListDefinition), 2);
        assert!(parsed.iter().any(
            |event| matches!(event, Event::FootnoteReference(label) if label.as_ref() == "note")
        ));
        assert!(parsed.iter().any(
            |event| matches!(event, Event::Start(Tag::FootnoteDefinition(label)) if label.as_ref() == "note")
        ));
        assert_eq!(
            parsed
                .iter()
                .filter(|event| matches!(event, Event::TaskListMarker(false)))
                .count(),
            1
        );
        assert_eq!(
            parsed
                .iter()
                .filter(|event| matches!(event, Event::TaskListMarker(true)))
                .count(),
            2
        );
        assert_well_formed(markdown);
    }

    #[test]
    fn recognizes_every_gfm_alert_kind_and_rejects_near_misses() {
        for (name, expected) in [
            ("NOTE", BlockQuoteKind::Note),
            ("TIP", BlockQuoteKind::Tip),
            ("IMPORTANT", BlockQuoteKind::Important),
            ("WARNING", BlockQuoteKind::Warning),
            ("CAUTION", BlockQuoteKind::Caution),
        ] {
            let markdown = format!("> [!{name}]\n> body");
            let parsed = events(&markdown);
            assert!(parsed.iter().any(
                |event| matches!(event, Event::Start(Tag::BlockQuote(Some(kind))) if *kind == expected)
            ));
            assert_well_formed(&markdown);
        }

        let lowercase = "> [!note]\n> lowercase";
        assert!(events(lowercase).iter().any(|event| matches!(
            event,
            Event::Start(Tag::BlockQuote(Some(BlockQuoteKind::Note)))
        )));
        assert_well_formed(lowercase);

        let unknown = "> [!UNKNOWN]\n> unknown";
        assert!(
            events(unknown)
                .iter()
                .any(|event| matches!(event, Event::Start(Tag::BlockQuote(None))))
        );
        assert_well_formed(unknown);
    }

    #[test]
    fn parses_inline_and_display_math_as_distinct_events() {
        let markdown = "inline $E = mc^2$, display $$\\sum_{i=1}^{n} i$$, escaped \\$literal\\$.";
        let events = events(markdown);

        assert!(events.iter().any(
            |event| matches!(event, Event::InlineMath(formula) if formula.as_ref() == "E = mc^2")
        ));
        assert!(events.iter().any(
            |event| matches!(event, Event::DisplayMath(formula) if formula.as_ref() == "\\sum_{i=1}^{n} i")
        ));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::InlineMath(_)))
                .count(),
            1
        );
        assert!(text(&events).contains("$literal$"));
        assert_well_formed(markdown);
    }

    #[test]
    fn preserves_unicode_and_reports_utf8_safe_source_offsets() {
        let markdown = "# 中文 👩🏽‍💻 e\u{301}\r\n\r\n[链接](https://例子.测试/路径?q=值)";
        let parsed = events(markdown);

        assert!(text(&parsed).contains("中文 👩🏽‍💻 e\u{301}"));
        assert!(text(&parsed).contains("链接"));
        assert!(parsed.iter().any(|event| matches!(
            event,
            Event::Start(Tag::Link { dest_url, .. })
                if dest_url.as_ref() == "https://例子.测试/路径?q=值"
        )));
        assert_well_formed(markdown);
    }

    #[test]
    fn every_prefix_of_streamed_markdown_is_safe_and_balanced() {
        let markdown = concat!(
            "# 流式 👩🏽‍💻\n\n",
            "> [!NOTE]\n> **bold _nested_** and $x^2$\n\n",
            "| A | B |\n|:-|:-:|\n| `a|b` | [链接](https://例子.测试) |\n\n",
            "```rust\nfn main() { println!(\"你好\"); }\n```\n",
        );

        for end in markdown
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(markdown.len()))
        {
            assert_well_formed(&markdown[..end]);
        }
    }

    #[test]
    fn malformed_and_incomplete_constructs_remain_safe() {
        for markdown in [
            "",
            "*",
            "**unfinished",
            "***crossing**",
            "`unterminated",
            "``code `",
            "```rust\nfn main()",
            "[link](",
            "[link](<unterminated",
            "![image][missing]",
            "<div",
            "<!-- unterminated",
            "$unterminated",
            "$$unterminated",
            "| a | b |\n| --- |",
            "> > >",
            "\0\u{1b}[2J",
        ] {
            assert_well_formed(markdown);
        }
    }

    #[test]
    fn capability_corpus_exercises_every_event_variant() {
        let markdown = concat!(
            "plain\n",
            "soft  \n",
            "hard\\\n",
            "next `code` $inline$ $$display$$ <i>html</i>[^note]\n\n",
            "***\n\n",
            "- [x] task\n\n",
            "<div>\nblock\n</div>\n\n",
            "[^note]: footnote\n",
        );
        let parsed = events(markdown);
        let kinds = parsed.iter().map(event_kind).collect::<BTreeSet<_>>();

        assert_eq!(
            kinds,
            BTreeSet::from([
                "code",
                "display_math",
                "end",
                "footnote_reference",
                "hard_break",
                "html",
                "inline_html",
                "inline_math",
                "rule",
                "soft_break",
                "start",
                "task_list_marker",
                "text",
            ])
        );
        assert_well_formed(markdown);
    }

    #[test]
    fn handles_pathological_delimiters_and_deep_containers() {
        let delimiter_storm = format!(
            "{}middle{}",
            "*_~^`[$".repeat(2_000),
            "$]`^~_*".repeat(2_000)
        );
        assert_well_formed(&delimiter_storm);

        let deep_quote = format!("{}deep\n", "> ".repeat(256));
        assert_well_formed(&deep_quote);
        assert_eq!(
            count_start(&events(&deep_quote), TagEnd::BlockQuote(None)),
            256
        );
    }

    #[test]
    fn documents_the_exact_extension_configuration() {
        let enabled = options();
        for option in [
            Options::ENABLE_TABLES,
            Options::ENABLE_FOOTNOTES,
            Options::ENABLE_STRIKETHROUGH,
            Options::ENABLE_TASKLISTS,
            Options::ENABLE_MATH,
            Options::ENABLE_GFM,
            Options::ENABLE_DEFINITION_LIST,
            Options::ENABLE_SUPERSCRIPT,
            Options::ENABLE_SUBSCRIPT,
        ] {
            assert!(enabled.contains(option), "{option:?}");
        }
        for option in [
            Options::ENABLE_SMART_PUNCTUATION,
            Options::ENABLE_HEADING_ATTRIBUTES,
            Options::ENABLE_YAML_STYLE_METADATA_BLOCKS,
            Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS,
            Options::ENABLE_OLD_FOOTNOTES,
            Options::ENABLE_WIKILINKS,
        ] {
            assert!(!enabled.contains(option), "{option:?}");
        }
    }

    #[test]
    fn disabled_extensions_do_not_silently_change_document_semantics() {
        let markdown = concat!(
            "---\ntitle: value\n---\n\n",
            "# heading {#id .class}\n\n",
            "\"straight quotes\" -- ... [[Wiki Page]]",
        );
        let parsed = events(markdown);
        let literal = text(&parsed);

        assert!(parsed.iter().any(|event| matches!(
            event,
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                id: None,
                classes,
                attrs,
            }) if classes.is_empty() && attrs.is_empty()
        )));
        assert!(
            !parsed
                .iter()
                .any(|event| matches!(event, Event::Start(Tag::MetadataBlock(_))))
        );
        assert!(!parsed.iter().any(|event| matches!(
            event,
            Event::Start(Tag::Link {
                link_type: LinkType::WikiLink { .. },
                ..
            })
        )));
        assert!(literal.contains("{#id .class}"));
        assert!(literal.contains("\"straight quotes\" -- ... [[Wiki Page]]"));
        assert_well_formed(markdown);
    }
}
