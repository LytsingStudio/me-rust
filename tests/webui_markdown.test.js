"use strict";

const { describe, expect, test } = require("bun:test");
const markdown = require("../src/webui/markdown.js");

function count(html, needle) {
  return html.split(needle).length - 1;
}

describe("WebUI Markdown renderer", () => {
  test("keeps loose repeated-one source in one ordered list", () => {
    const html = markdown.render("1. first\n\n1. second\n\n1. third\n\n1. fourth\n\n1. fifth");
    expect(count(html, "<ol>")).toBe(1);
    expect(count(html, "<li>")).toBe(5);
    expect(count(html, "</ol>")).toBe(1);
  });

  test("preserves ordered starts and nested mixed lists", () => {
    const html = markdown.render("3. third\n4. fourth\n   - nested\n     1. deep\n     2. deeper\n5. fifth");
    expect(html).toContain('<ol start="3">');
    expect(count(html, "<ol")).toBe(2);
    expect(count(html, "<ul>")).toBe(1);
    expect(html).toContain("deep");
  });

  test("renders task items without interactive controls", () => {
    const html = markdown.render("- [ ] pending\n- [x] complete\n- [X] complete too");
    expect(html).toContain('class="task-list-item task-pending"');
    expect(count(html, 'class="task-list-item task-completed"')).toBe(2);
    expect(html).not.toContain("[x]");
    expect(html).not.toContain("<input");
  });

  test("supports headings rules quotes and hard breaks", () => {
    const html = markdown.render("Heading\n=======\n\n## Two\n\n> outer\n>\n> > inner\n\n---\n\nline  \nbreak");
    expect(html).toContain("<h1>Heading</h1>");
    expect(html).toContain("<h2>Two</h2>");
    expect(count(html, "<blockquote>")).toBe(2);
    expect(html).toContain("<hr>");
    expect(html).toContain("line<br>\nbreak");
  });

  test("preserves conversational soft line breaks", () => {
    const html = markdown.render("第一行\n第二行\n第三行");
    expect(html).toContain("第一行<br>\n第二行<br>\n第三行");
  });

  test("handles long fences and keeps markup literal inside code", () => {
    const html = markdown.render("````rust\n``` is content\n<b>literal</b>\n````");
    expect(html).toContain('class="language-rust"');
    expect(html).toContain("``` is content");
    expect(html).toContain("&lt;b&gt;literal&lt;/b&gt;");
    expect(html).not.toContain("<b>literal</b>");
  });

  test("renders aligned tables with escaped pipes and inline markup", () => {
    const html = markdown.render("| left | center | right |\n| :--- | :---: | ---: |\n| a\\|b | `c\\|d` | **bold** |");
    expect(html).toContain("<table>");
    expect(html).toContain('style="text-align:center"');
    expect(html).toContain('style="text-align:right"');
    expect(html).toContain("a|b");
    expect(html).toContain("<code>c|d</code>");
    expect(html).toContain("<strong>bold</strong>");
  });

  test("linkifies safe URLs and blocks executable schemes", () => {
    const html = markdown.render("[safe](https://example.com/a?q=1) and https://openai.com\n\n[unsafe](javascript:alert(1))");
    expect(count(html, 'target="_blank"')).toBe(2);
    expect(count(html, 'rel="noopener noreferrer"')).toBe(2);
    expect(html).not.toContain('href="javascript:');
  });

  test("escapes raw HTML and adds safe image loading attributes", () => {
    const html = markdown.render('<script>alert(1)</script>\n\n![alt](https://example.com/image.png "title")');
    expect(html).toContain("&lt;script&gt;alert(1)&lt;/script&gt;");
    expect(html).not.toContain("<script>");
    expect(html).toContain('loading="lazy"');
    expect(html).toContain('decoding="async"');
    expect(html).toContain('referrerpolicy="no-referrer"');
  });

  test("handles complex inline delimiters escapes unicode and autolinks", () => {
    const html = markdown.render("***both*** foo_bar_baz ~~gone~~ ``code ` tick`` \\*literal\\* 中文👩🏽‍💻 <mail@example.com>");
    expect(html).toContain("<em><strong>both</strong></em>");
    expect(html).toContain("foo_bar_baz");
    expect(html).toContain("<s>gone</s>");
    expect(html).toContain("<code>code ` tick</code>");
    expect(html).toContain("*literal*");
    expect(html).toContain("中文👩🏽‍💻");
    expect(html).toContain('href="mailto:mail@example.com"');
  });

  test("renders paired CJK strong text across punctuation boundaries", () => {
    const html = markdown.render([
      "一句话：**别生吃、别半生不熟，炖熟煮透就完全可以放心吃。**",
      "",
      "豆橛子是对**芸豆（豆角、四季豆类）**的俗称。",
      "",
      "这是**（非常重要）**的提醒。",
      "",
      "- **列表中的完整粗体。**",
    ].join("\n"));
    expect(html).toContain("一句话：<strong>别生吃、别半生不熟，炖熟煮透就完全可以放心吃。</strong>");
    expect(html).toContain("对<strong>芸豆（豆角、四季豆类）</strong>的俗称");
    expect(html).toContain("这是<strong>（非常重要）</strong>的提醒");
    expect(html).toContain("<li><strong>列表中的完整粗体。</strong></li>");
    expect(count(html, "<strong>")).toBe(4);
  });

  test("does not rewrite CJK-like delimiters in protected or unmatched text", () => {
    const html = markdown.render([
      "`代码**（保持原样）**文本`",
      "",
      "\\**（转义保持原样）**文本",
      "",
      "```text",
      "围栏**（保持原样）**文本",
      "```",
      "",
      "未闭合**（保持字面量）",
    ].join("\n"));
    expect(html).toContain("<code>代码**（保持原样）**文本</code>");
    expect(html).toContain("**（转义保持原样）**文本");
    expect(html).toContain("围栏**（保持原样）**文本");
    expect(html).toContain("未闭合**（保持字面量）");
    expect(html).not.toContain("\u000b");
  });

  test("is deterministic and safe for every streamed prefix", () => {
    const source = "# title\n\n1. one\n\n1. two with **bold** and [link](https://example.com)\n\n```js\nconst value = `<tag>`;\n```";
    const boundaries = [...source].map((_, index) => index + 1);
    for (const end of boundaries) {
      const prefix = source.slice(0, end);
      expect(() => markdown.render(prefix)).not.toThrow();
      expect(markdown.render(prefix)).toBe(markdown.render(prefix));
    }
  });

  test("does not turn display-math-like text into unsafe HTML", () => {
    const html = markdown.render("Inline $E = mc^2$\n\n$$\\sum_{i=1}^{n} i = n(n+1)/2$$");
    expect(html).toContain("$E = mc^2$");
    expect(html).toContain("$$\\sum_{i=1}^{n} i = n(n+1)/2$$");
  });
});
