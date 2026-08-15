"use strict";

const { describe, expect, test } = require("bun:test");
const { readFileSync } = require("node:fs");
const { join } = require("node:path");

function loadDraftRuntime() {
  const source = readFileSync(join(import.meta.dir, "../src/webui/app.js"), "utf8");
  const eventBindings = source.indexOf("\nelements.tabs.querySelectorAll");
  if (eventBindings < 0) throw new Error("could not isolate WebUI draft runtime");
  const factory = new Function("document", "performance", "matchMedia", `${source.slice(0, eventBindings)}
    return { state, observeInputDraft };`);
  return factory(
    { querySelector: () => null },
    { now: () => 0 },
    () => ({ matches: false, addEventListener: () => {} }),
  );
}

describe("WebUI authoritative input draft synchronization", () => {
  test("a newer Runtime draft supersedes an in-flight local write", () => {
    const { state, observeInputDraft } = loadDraftRuntime();
    const store = { inputDraftRevision: 10 };
    state.draftSync.set("main", {
      desired: "",
      sent: "old local value",
      sending: true,
      paused: false,
      waiters: [],
    });

    expect(observeInputDraft({
      id: "main",
      input_draft_revision: 11,
      input_draft: "撤回后恢复的消息",
    }, store)).toBe(true);
    expect(store.inputDraftRevision).toBe(11);
    expect(state.drafts.get("main")).toBe("撤回后恢复的消息");
    expect(state.draftSync.get("main").desired).toBe("撤回后恢复的消息");
    expect(state.draftSync.get("main").sent).toBe("撤回后恢复的消息");
  });

  test("submission pause preserves text typed while the prompt is being accepted", () => {
    const { state, observeInputDraft } = loadDraftRuntime();
    const store = { inputDraftRevision: 4 };
    state.draftSync.set("main", {
      desired: "next message",
      sent: "",
      sending: false,
      paused: true,
      waiters: [],
    });

    expect(observeInputDraft({
      id: "main",
      input_draft_revision: 5,
      input_draft: "",
    }, store)).toBe(false);
    expect(store.inputDraftRevision).toBe(4);
    expect(state.draftSync.get("main").desired).toBe("next message");
  });
});
