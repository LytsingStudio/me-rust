"use strict";

const { describe, expect, test } = require("bun:test");
const { readFileSync } = require("node:fs");
const { join } = require("node:path");

function loadDraftRuntime() {
  const source = readFileSync(join(import.meta.dir, "../src/webui/app.js"), "utf8");
  const eventBindings = source.indexOf("\nelements.tabs.querySelectorAll");
  if (eventBindings < 0) throw new Error("could not isolate WebUI draft runtime");
  const factory = new Function("document", "performance", "matchMedia", `${source.slice(0, eventBindings)}
    return { state, elements, observeInputDraft, saveDraft, beginInputComposition, endInputComposition };`);
  const input = { value: "", style: {}, scrollHeight: 0 };
  return factory(
    { querySelector: (selector) => selector === "#prompt-input" ? input : null },
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
      inFlight: { expectedRevision: 10, content: "older local write" },
      pendingRemote: null,
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

  test("the Runtime echo of an in-flight write never replaces newer local input", () => {
    const { state, elements, observeInputDraft } = loadDraftRuntime();
    const store = { inputDraftRevision: 10 };
    state.selectedAgent = "main";
    state.drafts.set("main", "abcdef");
    elements.input.value = "abcdef";
    state.draftSync.set("main", {
      desired: "abcdef",
      sent: "",
      sending: true,
      paused: false,
      inFlight: { expectedRevision: 10, content: "abc" },
      pendingRemote: null,
      waiters: [],
    });

    expect(observeInputDraft({
      id: "main",
      input_draft_revision: 11,
      input_draft: "abc",
    }, store)).toBe(false);
    expect(store.inputDraftRevision).toBe(11);
    expect(elements.input.value).toBe("abcdef");
    expect(state.drafts.get("main")).toBe("abcdef");
    expect(state.draftSync.get("main").desired).toBe("abcdef");
    expect(state.draftSync.get("main").sent).toBe("abc");
  });

  test("IME composition is kept intact while a remote draft revision arrives", () => {
    const runtime = loadDraftRuntime();
    const { state, elements, observeInputDraft, saveDraft,
      beginInputComposition, endInputComposition } = runtime;
    const store = { inputDraftRevision: 5 };
    state.selectedAgent = "main";
    state.stores.set("main", store);
    state.drafts.set("main", "旧文本");
    elements.input.value = "旧文本";

    beginInputComposition();
    elements.input.value = "完整中文";
    saveDraft();
    expect(observeInputDraft({
      id: "main",
      input_draft_revision: 6,
      input_draft: "另一个页面的文本",
    }, store)).toBe(false);
    expect(elements.input.value).toBe("完整中文");
    expect(store.inputDraftRevision).toBe(5);

    state.draftSync.get("main").paused = true;
    endInputComposition();
    expect(store.inputDraftRevision).toBe(6);
    expect(elements.input.value).toBe("完整中文");
    expect(state.drafts.get("main")).toBe("完整中文");
    expect(state.draftSync.get("main").sent).toBe("另一个页面的文本");
    expect(state.draftSync.get("main").desired).toBe("完整中文");
    expect(state.draftSync.get("main").pendingRemote).toBe(null);
  });

  test("submission pause preserves text typed while the prompt is being accepted", () => {
    const { state, observeInputDraft } = loadDraftRuntime();
    const store = { inputDraftRevision: 4 };
    state.draftSync.set("main", {
      desired: "next message",
      sent: "",
      sending: false,
      paused: true,
      inFlight: null,
      pendingRemote: null,
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
