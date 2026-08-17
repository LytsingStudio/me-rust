(function (root, factory) {
  if (typeof module === "object" && module.exports) {
    module.exports = factory(require("./vendor/markdown-it.min.js"));
  } else {
    root.MeMarkdown = factory(root.markdownit);
  }
}(typeof globalThis !== "undefined" ? globalThis : this, function (markdownitFactory) {
  "use strict";

  if (typeof markdownitFactory !== "function") {
    throw new Error("Markdown renderer is unavailable");
  }

  const parser = markdownitFactory({
    html: false,
    linkify: true,
    breaks: true,
    typographer: false,
  });

  const defaultLinkOpen = parser.renderer.rules.link_open
    || ((tokens, index, options, environment, renderer) => renderer.renderToken(tokens, index, options));
  parser.renderer.rules.link_open = (tokens, index, options, environment, renderer) => {
    const href = tokens[index].attrGet("href") || "";
    if (/^https?:\/\//i.test(href)) {
      tokens[index].attrSet("target", "_blank");
      tokens[index].attrSet("rel", "noopener noreferrer");
    }
    return defaultLinkOpen(tokens, index, options, environment, renderer);
  };

  const defaultImage = parser.renderer.rules.image
    || ((tokens, index, options, environment, renderer) => renderer.renderToken(tokens, index, options));
  parser.renderer.rules.image = (tokens, index, options, environment, renderer) => {
    tokens[index].attrSet("loading", "lazy");
    tokens[index].attrSet("decoding", "async");
    tokens[index].attrSet("referrerpolicy", "no-referrer");
    return defaultImage(tokens, index, options, environment, renderer);
  };

  parser.renderer.rules.table_open = () => '<div class="markdown-table-wrap"><table>\n';
  parser.renderer.rules.table_close = () => "</table></div>\n";

  const CJK_EMPHASIS_SENTINEL = "\u000b";
  const CJK_CHARACTER = /[\u1100-\u11ff\u3040-\u30ff\u3100-\u312f\u3400-\u4dbf\u4e00-\u9fff\uac00-\ud7af\uf900-\ufaff]|[\u{20000}-\u{2fa1f}]/u;
  const WORD_CHARACTER = /[\p{L}\p{N}]/u;

  function isPunctuationLike(character) {
    return Boolean(character) && !WORD_CHARACTER.test(character) && !/\s/u.test(character);
  }

  function normalizeCjkEmphasis(source) {
    const markers = collectStrongMarkers(source);
    const open = [];
    const pairs = [];
    markers.forEach((marker, index) => {
      if (open.length && (marker.standardClose || marker.compatibilityClose)) {
        pairs.push([open.pop(), index]);
      } else if (marker.standardOpen || marker.compatibilityOpen) {
        open.push(index);
      }
    });
    pairs.forEach(([opening, closing]) => {
      const opener = markers[opening];
      const closer = markers[closing];
      opener.insertBefore = !opener.standardOpen && opener.compatibilityOpen;
      closer.insertAfter = !closer.standardClose && closer.compatibilityClose;
    });
    if (!markers.some((marker) => marker.insertBefore || marker.insertAfter)) return source;

    let output = "";
    let sourceCursor = 0;
    markers.forEach((marker) => {
      output += source.slice(sourceCursor, marker.offset);
      output += `${marker.insertBefore ? CJK_EMPHASIS_SENTINEL : ""}**${marker.insertAfter ? CJK_EMPHASIS_SENTINEL : ""}`;
      sourceCursor = marker.offset + 2;
    });
    return output + source.slice(sourceCursor);
  }

  function collectStrongMarkers(source) {
    const markers = [];
    let code = null;
    for (let cursor = 0; cursor < source.length;) {
      const character = source[cursor];
      if (character === "`" || character === "~") {
        let length = 1;
        while (source[cursor + length] === character) length += 1;
        const fencePosition = isFencePosition(source, cursor);
        if (code) {
          const closes = code.character === character
            && (code.fenced ? fencePosition && length >= code.length : length === code.length);
          if (closes) code = null;
          cursor += length;
          continue;
        }
        if (character === "`" || (character === "~" && fencePosition && length >= 3)) {
          code = { character, length, fenced: fencePosition && length >= 3 };
          cursor += length;
          continue;
        }
      }
      if (code) {
        cursor += source.codePointAt(cursor) > 0xffff ? 2 : 1;
        continue;
      }
      if (source.startsWith("**", cursor)
          && source[cursor - 1] !== "*" && source[cursor + 2] !== "*"
          && !isEscaped(source, cursor)
          && !insideLinkTarget(source, cursor)) {
        const preceding = Array.from(source.slice(0, cursor));
        const previous = preceding[preceding.length - 1] || "";
        const next = Array.from(source.slice(cursor + 2))[0] || "";
        const previousWhitespace = !previous || /\s/u.test(previous);
        const nextWhitespace = !next || /\s/u.test(next);
        const previousPunctuation = isPunctuationLike(previous);
        const nextPunctuation = isPunctuationLike(next);
        const standardOpen = !nextWhitespace
          && (!nextPunctuation || previousWhitespace || previousPunctuation);
        const standardClose = !previousWhitespace
          && (!previousPunctuation || nextWhitespace || nextPunctuation);
        const compatibilityOpen = CJK_CHARACTER.test(previous) && nextPunctuation;
        const compatibilityClose = previousPunctuation && CJK_CHARACTER.test(next);
        if (standardOpen || standardClose || compatibilityOpen || compatibilityClose) {
          markers.push({
            offset: cursor,
            standardOpen,
            standardClose,
            compatibilityOpen,
            compatibilityClose,
            insertBefore: false,
            insertAfter: false,
          });
        }
        cursor += 2;
        continue;
      }
      const width = source.codePointAt(cursor) > 0xffff ? 2 : 1;
      cursor += width;
    }
    return markers;
  }

  function isEscaped(source, index) {
    let slashCount = 0;
    for (let cursor = index - 1; cursor >= 0 && source[cursor] === "\\"; cursor -= 1) {
      slashCount += 1;
    }
    return slashCount % 2 === 1;
  }

  function isFencePosition(source, index) {
    const lineStart = source.lastIndexOf("\n", index - 1) + 1;
    const prefix = source.slice(lineStart, index);
    return prefix.length <= 3 && /^ {0,3}$/.test(prefix);
  }

  function insideLinkTarget(source, index) {
    const line = source.slice(source.lastIndexOf("\n", index - 1) + 1, index);
    const token = line.slice(Math.max(line.lastIndexOf(" "), line.lastIndexOf("\t")) + 1);
    if (token.includes("://")) return true;
    const open = line.lastIndexOf("](");
    return open >= 0 && open > line.lastIndexOf(")");
  }

  const defaultListItemOpen = parser.renderer.rules.list_item_open
    || ((tokens, index, options, environment, renderer) => renderer.renderToken(tokens, index, options));
  parser.renderer.rules.list_item_open = (tokens, index, options, environment, renderer) => {
    const itemLevel = tokens[index].level;
    for (let cursor = index + 1; cursor < tokens.length; cursor += 1) {
      const token = tokens[cursor];
      if (token.type === "list_item_close" && token.level === itemLevel) break;
      if (token.type !== "inline" || !token.children?.length) continue;
      const firstText = token.children.find((child) => child.type === "text");
      const task = firstText?.content.match(/^\[([ xX])\]\s+/);
      if (!task) break;
      firstText.content = firstText.content.slice(task[0].length);
      token.content = token.content.slice(task[0].length);
      tokens[index].attrJoin("class", `task-list-item ${task[1] === " " ? "task-pending" : "task-completed"}`);
      break;
    }
    return defaultListItemOpen(tokens, index, options, environment, renderer);
  };

  function render(source) {
    const normalized = normalizeCjkEmphasis(String(source || "").replace(/\r\n?/g, "\n"));
    return parser.render(normalized).split(CJK_EMPHASIS_SENTINEL).join("");
  }

  return Object.freeze({
    engine: "markdown-it 15.0.0",
    render,
  });
}));
