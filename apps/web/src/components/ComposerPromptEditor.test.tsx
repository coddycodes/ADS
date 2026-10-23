// @vitest-environment happy-dom

import { type ServerProviderAgent, type ServerProviderSkill, ThreadId } from "@bibcode/contracts";
import { serializeComposerReference } from "@bibcode/shared/composerReferences";
import { serializeComposerFileLink } from "@bibcode/shared/composerTrigger";
import {
  $createRangeSelection,
  $createTextNode,
  $getRoot,
  $getSelection,
  $isElementNode,
  $isRangeSelection,
  $setSelection,
  KEY_ARROW_DOWN_COMMAND,
  KEY_ARROW_LEFT_COMMAND,
  KEY_ARROW_RIGHT_COMMAND,
  KEY_ARROW_UP_COMMAND,
  KEY_BACKSPACE_COMMAND,
  KEY_ENTER_COMMAND,
  KEY_TAB_COMMAND,
  type ElementNode,
  type LexicalEditor,
  type LexicalNode,
} from "lexical";
import { act, createRef } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import {
  INLINE_TERMINAL_CONTEXT_PLACEHOLDER,
  type TerminalContextDraft,
} from "../lib/terminalContext";
import { type ComposerPromptEditorHandle, ComposerPromptEditor } from "./ComposerPromptEditor";

// ---------------------------------------------------------------------------
// Harness: renderToStaticMarkup never runs effects or imperative handles, so
// this suite queues them during render and flushes them afterwards. Post-render
// state setters are no-ops on the server renderer, which keeps this safe. The
// mocked HistoryPlugin doubles as a probe that captures the Lexical editor so
// tests can dispatch commands headlessly.
// ---------------------------------------------------------------------------

const harness = vi.hoisted(() => ({
  effects: [] as Array<() => unknown>,
  executed: [] as Array<() => unknown>,
  editors: [] as unknown[],
  useRealEffects: false,
  observedOnChangeUpdates: 0,
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  const queueEffect: typeof actual.useEffect = (effect, dependencies) => {
    if (harness.useRealEffects) {
      return actual.useEffect(effect, dependencies);
    }
    harness.effects.push(effect);
  };
  const queueLayoutEffect: typeof actual.useLayoutEffect = (effect, dependencies) => {
    if (harness.useRealEffects) {
      return actual.useLayoutEffect(effect, dependencies);
    }
    harness.effects.push(effect);
  };
  const queueImperativeHandle: typeof actual.useImperativeHandle = (ref, create, dependencies) => {
    if (harness.useRealEffects) {
      return actual.useImperativeHandle(ref, create, dependencies);
    }
    harness.effects.push(() => {
      if (typeof ref === "function") {
        ref(create());
        return;
      }
      if (ref && typeof ref === "object") {
        (ref as { current: unknown }).current = create();
      }
    });
  };
  // react-dom-server's useEffectEvent returns a function that throws when
  // invoked ("can't be called during rendering"); the suite drives DOM event
  // handlers after the render pass, so return the callback unchanged instead.
  const passthroughEffectEvent = ((callback: unknown) => callback) as typeof actual.useEffectEvent;
  return {
    ...actual,
    useEffect: queueEffect,
    useLayoutEffect: queueLayoutEffect,
    useImperativeHandle: queueImperativeHandle,
    useEffectEvent: passthroughEffectEvent,
  };
});

vi.mock("@lexical/react/LexicalHistoryPlugin", async () => {
  const { useLexicalComposerContext } = await import("@lexical/react/LexicalComposerContext");
  return {
    HistoryPlugin: function HistoryPluginProbe() {
      const [editor] = useLexicalComposerContext();
      harness.editors.push(editor);
      return null;
    },
  };
});

// The real OnChangePlugin registers its listener in an effect owned by the
// (external, unmocked-react) @lexical/react package, which never runs under
// the server renderer. Replicate its registration so `handleEditorChange`
// receives commits exactly like in the browser.
vi.mock("@lexical/react/LexicalOnChangePlugin", async () => {
  const { useLexicalComposerContext } = await import("@lexical/react/LexicalComposerContext");
  const { HISTORY_MERGE_TAG } = await import("lexical");
  const { useEffect } = await import("react");
  return {
    OnChangePlugin: function OnChangePluginProbe(props: {
      onChange: (
        editorState: import("lexical").EditorState,
        editor: import("lexical").LexicalEditor,
        tags: Set<string>,
      ) => void;
    }) {
      const [editor] = useLexicalComposerContext();
      useEffect(
        () =>
          editor.registerUpdateListener(({ editorState, prevEditorState, tags }) => {
            harness.observedOnChangeUpdates += 1;
            if (tags.has(HISTORY_MERGE_TAG) || prevEditorState.isEmpty()) {
              return;
            }
            props.onChange(editorState, editor, tags);
          }),
        [editor, props],
      );
      return null;
    },
  };
});

function flushQueuedEffects(): void {
  while (harness.effects.length > 0) {
    const pending = harness.effects.splice(0, harness.effects.length);
    for (const effect of pending) {
      harness.executed.push(effect);
      effect();
    }
  }
}

/**
 * Re-run every effect captured during the last flush. This simulates a
 * controlled re-render pass: effects observe refs that later editor activity
 * has mutated (e.g. the prompt snapshot), so value-reconciliation branches
 * execute exactly like they would when React re-fires the layout effect.
 */
function reflushExecutedEffects(): void {
  const executed = [...harness.executed];
  for (const effect of executed) {
    effect();
  }
}

function lastEditor(): LexicalEditor {
  const editor = harness.editors.at(-1);
  if (!editor) {
    throw new Error("expected a captured Lexical editor");
  }
  return editor as LexicalEditor;
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const MENTION_PATH = "src/app/main.ts";
const MENTION_SOURCE = serializeComposerReference(MENTION_PATH);

const agents: ServerProviderAgent[] = [
  {
    name: "reviewer",
    description: "Reviews implementation changes",
    invocation: "mention",
  },
  {
    name: "background",
    description: "Not directly mentionable",
  },
];

const skills: ServerProviderSkill[] = [
  {
    name: "refactor",
    displayName: "Refactor",
    shortDescription: "Refactor code safely",
    description: "Long refactor description",
    path: "/skills/refactor",
    scope: "project",
    enabled: true,
    invocation: "dollar",
  },
  {
    name: "docs",
    description: "   ",
    path: "/skills/docs",
    enabled: true,
    invocation: "dollar",
  },
  {
    name: "lint",
    description: "Lint the workspace",
    path: "/skills/lint",
    enabled: false,
    invocation: "dollar",
  },
];

function terminalContext(id: string): TerminalContextDraft {
  return {
    id,
    threadId: ThreadId.make("thread-1"),
    createdAt: "2026-03-17T18:42:05.449Z",
    terminalId: "term-1",
    terminalLabel: "Terminal 1",
    lineStart: 1,
    lineEnd: 3,
    text: "npm test output",
  };
}

interface RenderOptions {
  value?: string;
  cursor?: number;
  terminalContexts?: ReadonlyArray<TerminalContextDraft>;
  skills?: ReadonlyArray<ServerProviderSkill>;
  agents?: ReadonlyArray<ServerProviderAgent>;
  disabled?: boolean;
  placeholder?: string;
  className?: string;
  onCommandKeyDown?: (
    key: "ArrowDown" | "ArrowUp" | "Enter" | "Tab",
    event: KeyboardEvent,
  ) => boolean;
}

function renderEditor(options: RenderOptions = {}) {
  const editorRef = createRef<ComposerPromptEditorHandle>();
  const onChange = vi.fn();
  const onRemoveTerminalContext = vi.fn();
  const onPaste = vi.fn();
  const markup = renderToStaticMarkup(
    <ComposerPromptEditor
      value={options.value ?? ""}
      cursor={options.cursor ?? 0}
      terminalContexts={options.terminalContexts ?? []}
      skills={options.skills ?? []}
      agents={options.agents ?? []}
      disabled={options.disabled ?? false}
      placeholder={options.placeholder ?? "Ask anything"}
      onRemoveTerminalContext={onRemoveTerminalContext}
      onChange={onChange}
      onPaste={onPaste}
      editorRef={editorRef}
      {...(options.onCommandKeyDown ? { onCommandKeyDown: options.onCommandKeyDown } : {})}
      {...(options.className ? { className: options.className } : {})}
    />,
  );
  const editor = lastEditor();
  // Capture root listeners registered by plugins so tests can hand them a
  // fake root element and drive its DOM event handlers headlessly.
  const rootListeners: Array<(root: HTMLElement | null, prev: HTMLElement | null) => void> = [];
  const originalRegisterRootListener = editor.registerRootListener.bind(editor);
  editor.registerRootListener = ((listener: (typeof rootListeners)[number]) => {
    rootListeners.push(listener);
    return originalRegisterRootListener(listener);
  }) as typeof editor.registerRootListener;
  flushQueuedEffects();
  // Without a root element Lexical defers commits to a microtask and skips
  // DOM reconciliation. Flip the same flag `@lexical/headless` uses and force
  // a synchronous (discrete) commit of the initialization update so reads see
  // the initialized state immediately.
  (editor as unknown as { _headless: boolean })._headless = true;
  commitUpdates(editor);
  return {
    markup,
    editor,
    editorRef,
    onChange,
    onRemoveTerminalContext,
    onPaste,
    rootListeners,
  };
}

interface FakeRootElement {
  element: HTMLElement;
  focus: ReturnType<typeof vi.fn>;
  dispatch: (type: string, event: unknown) => void;
}

function createFakeRootElement(): FakeRootElement {
  const listeners = new Map<string, (event: unknown) => void>();
  const focus = vi.fn();
  const element = {
    focus,
    addEventListener: (type: string, handler: unknown) => {
      listeners.set(type, handler as (event: unknown) => void);
    },
    removeEventListener: () => {},
  };
  return {
    element: element as unknown as HTMLElement,
    focus,
    dispatch: (type, event) => {
      const handler = listeners.get(type);
      if (!handler) {
        throw new Error(`no ${type} listener attached`);
      }
      handler(event);
    },
  };
}

function keyEvent(
  overrides: Partial<{
    isComposing: boolean;
    keyCode: number;
    shiftKey: boolean;
  }> = {},
) {
  const event = {
    isComposing: false,
    keyCode: 0,
    shiftKey: false,
    preventDefault: vi.fn(),
    stopPropagation: vi.fn(),
    ...overrides,
  };
  return event as unknown as KeyboardEvent & {
    preventDefault: ReturnType<typeof vi.fn>;
    stopPropagation: ReturnType<typeof vi.fn>;
  };
}

/** Force Lexical's microtask-deferred pending update to commit synchronously. */
function commitUpdates(editor: LexicalEditor): void {
  editor.update(() => {}, { discrete: true });
}

function readRootText(editor: LexicalEditor): string {
  commitUpdates(editor);
  return editor.getEditorState().read(() => $getRoot().getTextContent());
}

function readAnchor(editor: LexicalEditor): { type: string; offset: number } {
  commitUpdates(editor);
  return editor.getEditorState().read(() => {
    const selection = $getSelection();
    if (!$isRangeSelection(selection)) throw new Error("expected range selection");
    return { type: selection.anchor.type, offset: selection.anchor.offset };
  });
}

function $paragraph(): ElementNode {
  const paragraph = $getRoot().getFirstChild();
  if (!$isElementNode(paragraph)) {
    throw new Error("expected the root paragraph");
  }
  return paragraph;
}

function setCollapsedSelection(
  editor: LexicalEditor,
  resolvePoint: () => { key: string; offset: number; type: "text" | "element" },
): void {
  editor.update(
    () => {
      const point = resolvePoint();
      const selection = $createRangeSelection();
      selection.anchor.set(point.key, point.offset, point.type);
      selection.focus.set(point.key, point.offset, point.type);
      $setSelection(selection);
    },
    { discrete: true },
  );
}

function setRangeSelection(
  editor: LexicalEditor,
  resolvePoints: () => {
    anchor: { key: string; offset: number; type: "text" | "element" };
    focus: { key: string; offset: number; type: "text" | "element" };
  },
): void {
  editor.update(
    () => {
      const points = resolvePoints();
      const selection = $createRangeSelection();
      selection.anchor.set(points.anchor.key, points.anchor.offset, points.anchor.type);
      selection.focus.set(points.focus.key, points.focus.offset, points.focus.type);
      $setSelection(selection);
    },
    { discrete: true },
  );
}

/**
 * Place a collapsed text point directly on an inline token, the way a browser
 * selection can land inside a decorator span. `PointType.set` validates
 * against decorator nodes and selection transforms expect text-point nodes to
 * expose `selectionTransform`, so assign the fields directly and give the node
 * prototype the TextNode no-op.
 */
function setDoctoredTokenSelection(editor: LexicalEditor, tokenType: string, offset: number): void {
  editor.update(
    () => {
      const children = $paragraph().getChildren();
      const token = children.find((child) => child.getType() === tokenType);
      if (!token) throw new Error(`expected ${tokenType} node`);
      const prototype = Object.getPrototypeOf(token) as {
        selectionTransform?: () => void;
      };
      if (typeof prototype.selectionTransform !== "function") {
        prototype.selectionTransform = () => {};
      }
      const selection = $createRangeSelection();
      for (const point of [selection.anchor, selection.focus]) {
        point.key = token.getKey();
        point.offset = offset;
        point.type = "text";
      }
      $setSelection(selection);
    },
    { discrete: true },
  );
}

async function flushMicrotasks(): Promise<void> {
  await new Promise((resolve) => {
    setTimeout(resolve, 0);
  });
}

beforeEach(() => {
  harness.useRealEffects = false;
  harness.effects.length = 0;
  harness.executed.length = 0;
  harness.editors.length = 0;
  harness.observedOnChangeUpdates = 0;
});

afterEach(() => {
  harness.useRealEffects = false;
  vi.unstubAllGlobals();
});

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

describe("ComposerPromptEditor rendering", () => {
  it("renders the content editable with an aria placeholder and visible hint", () => {
    const { markup } = renderEditor({ placeholder: "Ask anything, @tag files" });

    expect(markup).toContain('data-testid="composer-editor"');
    expect(markup).toContain('aria-placeholder="Ask anything, @tag files"');
    // Empty prompt without terminal contexts shows the styled hint overlay.
    expect(markup).toContain("pointer-events-none");
  });

  it("hides the placeholder overlay when terminal contexts exist and applies className", () => {
    const { markup } = renderEditor({
      value: `${INLINE_TERMINAL_CONTEXT_PLACEHOLDER} `,
      terminalContexts: [terminalContext("ctx-1")],
      placeholder: "Hidden hint",
      className: "max-sm:pb-11",
    });

    expect(markup).toContain("max-sm:pb-11");
    expect(markup).not.toContain("pointer-events-none absolute inset-0");
  });

  it("initializes the editor state from the collapsed prompt", () => {
    const value = `Check ${MENTION_SOURCE} then @reviewer and $refactor\nrun ${INLINE_TERMINAL_CONTEXT_PLACEHOLDER} tail ${INLINE_TERMINAL_CONTEXT_PLACEHOLDER}`;
    const { editor } = renderEditor({
      value,
      terminalContexts: [terminalContext("ctx-1")],
      skills,
      agents,
    });

    // The orphan (second) terminal placeholder has no draft and is dropped.
    const expected = `Check ${MENTION_SOURCE} then @reviewer and $refactor\nrun ${INLINE_TERMINAL_CONTEXT_PLACEHOLDER} tail `;
    expect(readRootText(editor)).toBe(expected);

    const types = editor.getEditorState().read(() =>
      $paragraph()
        .getChildren()
        .map((child) => child.getType()),
    );
    expect(types).toContain("composer-mention");
    expect(types).toContain("composer-agent");
    expect(types).toContain("composer-skill");
    expect(types).toContain("composer-terminal-context");
    expect(types).toContain("linebreak");
  });

  it("reconstructs an EOF native file reference without mentionable agents", () => {
    const value = "Inspect @src/main.ts";
    const { editor } = renderEditor({ value });

    expect(readRootText(editor)).toBe(value);
    expect(
      editor.getEditorState().read(() =>
        $paragraph()
          .getChildren()
          .map((child) => child.getType()),
      ),
    ).toContain("composer-mention");
  });

  it("reconstructs native references adjacent to terminal context placeholders", () => {
    const value = `Inspect @src/before.ts${INLINE_TERMINAL_CONTEXT_PLACEHOLDER}@src/after.ts `;
    const { editor } = renderEditor({
      value,
      terminalContexts: [terminalContext("ctx-between")],
    });

    expect(readRootText(editor)).toBe(value);
    expect(
      editor.getEditorState().read(() =>
        $paragraph()
          .getChildren()
          .map((child) => child.getType()),
      ),
    ).toEqual([
      "text",
      "composer-mention",
      "composer-terminal-context",
      "composer-mention",
      "text",
    ]);
  });

  it.each([
    { label: "missing", inventory: [] },
    {
      label: "disabled",
      inventory: [{ ...skills[0]!, enabled: false }],
    },
    {
      label: "slash-only",
      inventory: [{ ...skills[0]!, invocation: "slash" as const }],
    },
  ])("keeps $label provider dollar expressions as plain text", ({ inventory }) => {
    const source = "use $refactor now";
    const { editor, editorRef } = renderEditor({
      value: source,
      cursor: source.length,
      skills: inventory,
    });
    expect(readRootText(editor)).toBe(source);
    const skillTypes = editor.getEditorState().read(() =>
      $paragraph()
        .getChildren()
        .filter((child) => child.getType() === "composer-skill")
        .map((child) => child.getTextContent()),
    );
    expect(skillTypes).toEqual([]);
    expect(editorRef.current?.readSnapshot()).toMatchObject({
      cursor: source.length,
      expandedCursor: source.length,
    });
  });
});

// ---------------------------------------------------------------------------
// Serialization round-trip (exportJSON / importJSON)
// ---------------------------------------------------------------------------

interface SerializedTokenShape {
  type?: string;
  path?: string;
  agentName?: string;
  agentDescription?: string;
  skillName?: string;
  skillLabel?: string;
  skillDescription?: string;
  context?: { id?: string };
  children?: SerializedTokenShape[];
}

describe("ComposerPromptEditor serialization", () => {
  it("exports mention, agent, skill, and terminal context nodes to JSON and reimports them", () => {
    const value = `see ${MENTION_SOURCE} ask @reviewer and $refactor with $docs at ${INLINE_TERMINAL_CONTEXT_PLACEHOLDER}`;
    const { editor } = renderEditor({
      value,
      terminalContexts: [terminalContext("ctx-json")],
      skills,
      agents,
    });

    const serialized = editor.getEditorState().toJSON();
    const paragraph = (serialized.root as SerializedTokenShape).children?.[0];
    const children = paragraph?.children ?? [];
    const mention = children.find((child) => child.type === "composer-mention");
    const agent = children.find((child) => child.type === "composer-agent");
    const skillNodes = children.filter((child) => child.type === "composer-skill");
    const terminal = children.find((child) => child.type === "composer-terminal-context");

    expect(mention?.path).toBe(MENTION_PATH);
    expect(agent?.agentName).toBe("reviewer");
    expect(agent?.agentDescription).toBe("Reviews implementation changes");
    expect(skillNodes.map((node) => node.skillName)).toEqual(["refactor", "docs"]);
    // Metadata-backed skill keeps its label/description; docs has neither.
    expect(skillNodes[0]?.skillLabel).toBe("Refactor");
    expect(skillNodes[0]?.skillDescription).toBe("Refactor code safely");
    expect(skillNodes[1]?.skillDescription).toBeUndefined();
    expect(terminal?.context?.id).toBe("ctx-json");

    const reparsed = editor.parseEditorState(serialized);
    expect(reparsed.read(() => $getRoot().getTextContent())).toBe(value);
    expect(
      reparsed.read(() =>
        $paragraph()
          .getChildren()
          .map((child) => child.getType()),
      ),
    ).toEqual(expect.arrayContaining(["composer-mention", "composer-agent", "composer-skill"]));
  });

  it("serializes native file references and canonicalizes legacy markdown drafts", () => {
    const quotedPath = "docs/My File.md";
    const legacyPath = "legacy/file.ts";
    const value = `${MENTION_SOURCE} ${serializeComposerReference(quotedPath)} ${serializeComposerFileLink(legacyPath)} `;
    const { editor } = renderEditor({ value });

    expect(readRootText(editor)).toBe(
      `${MENTION_SOURCE} ${serializeComposerReference(quotedPath)} ${serializeComposerReference(legacyPath)} `,
    );
    const fileTexts = editor.getEditorState().read(() =>
      $paragraph()
        .getChildren()
        .filter((child) => child.getType() === "composer-mention")
        .map((child) => child.getTextContent()),
    );
    expect(fileTexts).toEqual(["@src/app/main.ts", '@"docs/My File.md"', "@legacy/file.ts"]);
  });

  it("normalizes serialized skill names that still carry the $ sigil", () => {
    const { editor } = renderEditor({ value: "" });
    const reparsed = editor.parseEditorState({
      root: {
        children: [
          {
            children: [
              {
                skillName: "$docs",
                type: "composer-skill",
                version: 1,
              },
            ],
            direction: null,
            format: "",
            indent: 0,
            type: "paragraph",
            version: 1,
          },
        ],
        direction: null,
        format: "",
        indent: 0,
        type: "root",
        version: 1,
      },
    } as unknown as Parameters<LexicalEditor["parseEditorState"]>[0]);

    expect(reparsed.read(() => $getRoot().getTextContent())).toBe("$docs");
  });
});

// ---------------------------------------------------------------------------
// Node behaviors (decorators, DOM factories)
// ---------------------------------------------------------------------------

describe("ComposerPromptEditor inline token nodes", () => {
  function renderTokens() {
    return renderEditor({
      value: `${MENTION_SOURCE} @reviewer $refactor $docs ${INLINE_TERMINAL_CONTEXT_PLACEHOLDER}`,
      terminalContexts: [terminalContext("ctx-chip")],
      skills,
      agents,
    });
  }

  function tokenNodes(editor: LexicalEditor): {
    mention: LexicalNode;
    agent: LexicalNode;
    skillWithDescription: LexicalNode;
    skillWithoutDescription: LexicalNode;
    terminal: LexicalNode;
  } {
    return editor.getEditorState().read(() => {
      const children = $paragraph().getChildren();
      const mention = children.find((child) => child.getType() === "composer-mention");
      const agent = children.find((child) => child.getType() === "composer-agent");
      const skillNodes = children.filter((child) => child.getType() === "composer-skill");
      const terminal = children.find((child) => child.getType() === "composer-terminal-context");
      if (
        !mention ||
        !agent ||
        skillNodes.length !== 2 ||
        !skillNodes[0] ||
        !skillNodes[1] ||
        !terminal
      ) {
        throw new Error("expected inline token fixtures");
      }
      return {
        mention,
        agent,
        skillWithDescription: skillNodes[0],
        skillWithoutDescription: skillNodes[1],
        terminal,
      };
    });
  }

  it("creates wrapper DOM spans and never updates them in place", () => {
    vi.stubGlobal("document", {
      documentElement: { classList: { contains: () => false } },
      createElement: () => ({ className: "" }),
    });
    const { editor } = renderTokens();
    const nodes = tokenNodes(editor);

    for (const node of [nodes.mention, nodes.agent, nodes.skillWithDescription, nodes.terminal]) {
      const decorator = node as unknown as {
        createDOM: () => { className: string };
        updateDOM: () => boolean;
        isInline: () => boolean;
      };
      expect(decorator.createDOM().className).toBe("inline-flex align-middle leading-none");
      expect(decorator.updateDOM()).toBe(false);
      expect(decorator.isInline()).toBe(true);
    }
  });

  it("decorates tokens with chips for mention, agent, skill, and terminal context", () => {
    vi.stubGlobal("document", {
      documentElement: { classList: { contains: () => true } },
      createElement: () => ({ className: "" }),
    });
    const { editor } = renderTokens();
    const nodes = tokenNodes(editor);
    const decorate = (node: LexicalNode) =>
      renderToStaticMarkup((node as unknown as { decorate: () => React.ReactElement }).decorate());

    const mentionMarkup = decorate(nodes.mention);
    expect(mentionMarkup).toContain('data-composer-mention-chip="true"');
    expect(mentionMarkup).toContain("main.ts");

    const agentMarkup = decorate(nodes.agent);
    expect(agentMarkup).toContain('data-composer-agent-chip="true"');
    expect(agentMarkup).toContain("reviewer");

    const describedSkillMarkup = decorate(nodes.skillWithDescription);
    expect(describedSkillMarkup).toContain('data-composer-skill-chip="true"');
    expect(describedSkillMarkup).toContain("Refactor");

    // The docs skill resolves no description (whitespace only) so it renders
    // the bare chip without a tooltip wrapper; its label is the title-cased
    // fallback of the skill name.
    const bareSkillMarkup = decorate(nodes.skillWithoutDescription);
    expect(bareSkillMarkup).toContain('data-composer-skill-chip="true"');
    expect(bareSkillMarkup).toContain("Docs");

    const terminalMarkup = decorate(nodes.terminal);
    expect(terminalMarkup).toContain("Terminal 1");
  });

  it("reconstructs dollar text when the active provider skill inventory changes", async () => {
    harness.useRealEffects = true;
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    const editorRef = createRef<ComposerPromptEditorHandle>();
    const onChange = vi.fn();
    const source = "use $refactor now";
    const supportedCursor = "use ".length + 1 + " now".length;
    const renderWithSkills = (nextSkills: ReadonlyArray<ServerProviderSkill>) => (
      <ComposerPromptEditor
        value={source}
        cursor={supportedCursor}
        terminalContexts={[]}
        skills={nextSkills}
        agents={[]}
        disabled={false}
        placeholder="Ask anything"
        onRemoveTerminalContext={vi.fn()}
        onChange={onChange}
        onPaste={vi.fn()}
        editorRef={editorRef}
      />
    );

    try {
      await act(async () => root.render(renderWithSkills(skills)));
      const editor = lastEditor();
      expect(container.querySelector('[data-composer-skill-chip="true"]')).not.toBeNull();
      expect(editorRef.current?.readSnapshot()).toMatchObject({
        cursor: supportedCursor,
        expandedCursor: source.length,
      });

      await act(async () => root.render(renderWithSkills([])));

      expect(lastEditor()).toBe(editor);
      expect(container.querySelector('[data-composer-skill-chip="true"]')).toBeNull();
      expect(editorRef.current?.readSnapshot()).toMatchObject({
        value: source,
        cursor: source.length,
        expandedCursor: source.length,
      });
      expect(onChange).not.toHaveBeenCalled();
    } finally {
      await act(async () => root.unmount());
      container.remove();
      harness.useRealEffects = false;
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });

  it("leaves focus where it is when skills arrive while the composer is unfocused", async () => {
    harness.useRealEffects = true;
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const container = document.createElement("div");
    const outside = document.createElement("button");
    outside.textContent = "Activity summary";
    document.body.append(outside, container);
    const root = createRoot(container);
    const editorRef = createRef<ComposerPromptEditorHandle>();
    const spies: Array<{ mockRestore: () => void }> = [];
    const source = "use $refactor now";
    const renderWithSkills = (nextSkills: ReadonlyArray<ServerProviderSkill>) => (
      <ComposerPromptEditor
        value={source}
        cursor={source.length}
        terminalContexts={[]}
        skills={nextSkills}
        agents={[]}
        disabled={false}
        placeholder="Ask anything"
        onRemoveTerminalContext={vi.fn()}
        onChange={vi.fn()}
        onPaste={vi.fn()}
        editorRef={editorRef}
      />
    );

    try {
      await act(async () => root.render(renderWithSkills([])));
      outside.focus();
      expect(document.activeElement).toBe(outside);
      const editorRoot = container.querySelector('[contenteditable="true"]');
      const domSelection = window.getSelection();
      if (!editorRoot || !domSelection) throw new Error("The composer did not render.");
      // Browsers focus an editable element when the page's selection is placed inside it, which
      // happy-dom does not emulate, so this watches for the selection and focus calls directly.
      const placeSelection = vi.spyOn(domSelection, "setBaseAndExtent");
      const focus = vi.spyOn(HTMLElement.prototype, "focus");
      spies.push(placeSelection, focus);

      // Provider capabilities load after the thread opens; a keyboard user may already be on
      // another control, and the refreshed chips must not pull focus into the composer.
      await act(async () => root.render(renderWithSkills(skills)));

      expect(container.querySelector('[data-composer-skill-chip="true"]')).not.toBeNull();
      expect(document.activeElement).toBe(outside);
      expect(placeSelection).not.toHaveBeenCalled();
      expect(focus.mock.contexts.filter((element) => element === editorRoot)).toEqual([]);
    } finally {
      for (const spy of spies) spy.mockRestore();
      await act(async () => root.unmount());
      container.remove();
      outside.remove();
      harness.useRealEffects = false;
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });

  it("drops a selection left in the unfocused composer instead of moving its caret", async () => {
    harness.useRealEffects = true;
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const container = document.createElement("div");
    const outside = document.createElement("button");
    document.body.append(outside, container);
    const root = createRoot(container);
    const editorRef = createRef<ComposerPromptEditorHandle>();
    const source = "use $refactor now";
    const cursor = "use ".length;
    const renderWithSkills = (nextSkills: ReadonlyArray<ServerProviderSkill>) => (
      <ComposerPromptEditor
        value={source}
        cursor={cursor}
        terminalContexts={[]}
        skills={nextSkills}
        agents={[]}
        disabled={false}
        placeholder="Ask anything"
        onRemoveTerminalContext={vi.fn()}
        onChange={vi.fn()}
        onPaste={vi.fn()}
        editorRef={editorRef}
      />
    );

    try {
      await act(async () => root.render(renderWithSkills([])));
      const editorRoot = container.querySelector('[contenteditable="true"]');
      const text = editorRoot?.querySelector("span")?.firstChild;
      const domSelection = window.getSelection();
      if (!editorRoot || !text || !domSelection) throw new Error("The composer did not render.");
      // The user typed in the composer and then clicked another control: the page's range
      // stays in the editor while focus moves on.
      domSelection.setBaseAndExtent(text, cursor, text, cursor);
      outside.focus();

      await act(async () => root.render(renderWithSkills(skills)));

      // A browser relocates the range into the rebuilt paragraph and reports it back through
      // selectionchange; happy-dom leaves it on the detached text node and fires nothing. So
      // this checks the contract: the leftover range is removed before the rebuild, and the
      // saved caret and focus are unchanged.
      expect(domSelection.rangeCount).toBe(0);
      expect(editorRef.current?.readSnapshot()).toMatchObject({ value: source, cursor });
      expect(document.activeElement).toBe(outside);
    } finally {
      await act(async () => root.unmount());
      container.remove();
      outside.remove();
      harness.useRealEffects = false;
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });

  it("updates the same editor when provider agent metadata changes", async () => {
    harness.useRealEffects = true;
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    const editorRef = createRef<ComposerPromptEditorHandle>();
    const onChange = vi.fn();
    const source = "ask @reviewer next";
    const cursor = "ask ".length + 1;
    const renderWithAgents = (nextAgents: ReadonlyArray<ServerProviderAgent>) => (
      <ComposerPromptEditor
        value={source}
        cursor={cursor}
        terminalContexts={[]}
        skills={[]}
        agents={nextAgents}
        disabled={false}
        placeholder="Ask anything"
        onRemoveTerminalContext={vi.fn()}
        onChange={onChange}
        onPaste={vi.fn()}
        editorRef={editorRef}
      />
    );

    try {
      await act(async () => root.render(renderWithAgents([])));
      const initialEditor = lastEditor();
      expect(container.querySelector('[data-composer-mention-chip="true"]')).not.toBeNull();
      expect(editorRef.current?.readSnapshot()).toMatchObject({
        value: source,
        cursor,
      });

      harness.observedOnChangeUpdates = 0;
      await act(async () => root.render(renderWithAgents(agents)));
      expect(lastEditor()).toBe(initialEditor);
      expect(harness.observedOnChangeUpdates).toBeGreaterThan(0);
      expect(container.querySelector('[data-composer-agent-chip="true"]')).not.toBeNull();
      expect(container.querySelector('[data-composer-mention-chip="true"]')).toBeNull();
      expect(editorRef.current?.readSnapshot()).toMatchObject({
        value: source,
        cursor,
      });

      await act(async () => root.render(renderWithAgents([])));
      expect(lastEditor()).toBe(initialEditor);
      expect(container.querySelector('[data-composer-mention-chip="true"]')).not.toBeNull();
      expect(container.querySelector('[data-composer-agent-chip="true"]')).toBeNull();
      expect(editorRef.current?.readSnapshot()).toMatchObject({
        value: source,
        cursor,
      });
      expect(onChange).not.toHaveBeenCalled();
    } finally {
      await act(async () => root.unmount());
      container.remove();
      harness.useRealEffects = false;
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });

  it("preserves a focused confirmed EOF reference across file and agent refreshes", async () => {
    harness.useRealEffects = true;
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    const editorRef = createRef<ComposerPromptEditorHandle>();
    const onChange = vi.fn();
    const source = "ask @reviewer";
    const cursor = "ask ".length + 1;
    const renderWithAgents = (nextAgents: ReadonlyArray<ServerProviderAgent>) => (
      <ComposerPromptEditor
        value={source}
        cursor={cursor}
        terminalContexts={[]}
        skills={[]}
        agents={nextAgents}
        disabled={false}
        placeholder="Ask anything"
        onRemoveTerminalContext={vi.fn()}
        onChange={onChange}
        onPaste={vi.fn()}
        editorRef={editorRef}
      />
    );

    try {
      await act(async () => root.render(renderWithAgents([])));
      const editor = lastEditor();
      await act(async () => editorRef.current?.focusAt(cursor));
      onChange.mockClear();
      expect(container.querySelector('[data-composer-mention-chip="true"]')).not.toBeNull();
      expect(editorRef.current?.readSnapshot()).toMatchObject({
        value: source,
        cursor,
      });

      harness.observedOnChangeUpdates = 0;
      await act(async () => root.render(renderWithAgents(agents)));
      expect(lastEditor()).toBe(editor);
      expect(harness.observedOnChangeUpdates).toBeGreaterThan(0);
      expect(container.querySelector('[data-composer-agent-chip="true"]')).not.toBeNull();
      expect(container.querySelector('[data-composer-mention-chip="true"]')).toBeNull();
      expect(editorRef.current?.readSnapshot()).toMatchObject({
        value: source,
        cursor,
      });
      expect(onChange).not.toHaveBeenCalled();

      harness.observedOnChangeUpdates = 0;
      await act(async () => root.render(renderWithAgents([])));
      expect(lastEditor()).toBe(editor);
      expect(harness.observedOnChangeUpdates).toBeGreaterThan(0);
      expect(container.querySelector('[data-composer-mention-chip="true"]')).not.toBeNull();
      expect(container.querySelector('[data-composer-agent-chip="true"]')).toBeNull();
      expect(editorRef.current?.readSnapshot()).toMatchObject({
        value: source,
        cursor,
      });
      expect(onChange).not.toHaveBeenCalled();
    } finally {
      await act(async () => root.unmount());
      container.remove();
      harness.useRealEffects = false;
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });

  it("keeps a focused incomplete EOF query as text during metadata refresh", async () => {
    harness.useRealEffects = true;
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    const editorRef = createRef<ComposerPromptEditorHandle>();
    const onChange = vi.fn();
    const initialSource = "ask ";
    const typedSource = "ask @pac";
    const typedCursor = typedSource.length;
    const renderEditorWith = (
      value: string,
      cursor: number,
      nextAgents: ReadonlyArray<ServerProviderAgent>,
    ) => (
      <ComposerPromptEditor
        value={value}
        cursor={cursor}
        terminalContexts={[]}
        skills={[]}
        agents={nextAgents}
        disabled={false}
        placeholder="Ask anything"
        onRemoveTerminalContext={vi.fn()}
        onChange={onChange}
        onPaste={vi.fn()}
        editorRef={editorRef}
      />
    );

    try {
      await act(async () => root.render(renderEditorWith(initialSource, initialSource.length, [])));
      const editor = lastEditor();
      editorRef.current?.focusAtEnd();
      onChange.mockClear();

      await act(async () => {
        editor.update(
          () => {
            const selection = $getSelection();
            if (!$isRangeSelection(selection)) {
              throw new Error("expected a focused range selection");
            }
            selection.insertText("@pac");
          },
          { discrete: true },
        );
      });
      expect(onChange).toHaveBeenLastCalledWith(typedSource, typedCursor, typedCursor, false, []);

      await act(async () => root.render(renderEditorWith(typedSource, typedCursor, [])));
      onChange.mockClear();
      harness.observedOnChangeUpdates = 0;
      await act(async () => root.render(renderEditorWith(typedSource, typedCursor, agents)));

      expect(lastEditor()).toBe(editor);
      expect(harness.observedOnChangeUpdates).toBeGreaterThan(0);
      expect(document.activeElement).toBe(
        container.querySelector('[data-testid="composer-editor"]'),
      );
      expect(container.querySelector('[data-composer-mention-chip="true"]')).toBeNull();
      expect(container.querySelector('[data-composer-agent-chip="true"]')).toBeNull();
      expect(editorRef.current?.readSnapshot()).toMatchObject({
        value: typedSource,
        cursor: typedCursor,
      });
      expect(onChange).not.toHaveBeenCalled();
    } finally {
      await act(async () => root.unmount());
      container.remove();
      harness.useRealEffects = false;
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });
});

// ---------------------------------------------------------------------------
// Imperative handle
// ---------------------------------------------------------------------------

describe("ComposerPromptEditor imperative handle", () => {
  it("reads a snapshot with collapsed and expanded cursors plus context ids", () => {
    const value = `${MENTION_SOURCE} tail ${INLINE_TERMINAL_CONTEXT_PLACEHOLDER}`;
    const { editor, editorRef } = renderEditor({
      value,
      cursor: 2,
      terminalContexts: [terminalContext("ctx-snap")],
    });

    const handle = editorRef.current;
    expect(handle).not.toBeNull();
    const snapshot = handle!.readSnapshot();
    expect(snapshot.value).toBe(value);
    expect(snapshot.terminalContextIds).toEqual(["ctx-snap"]);
    // No live selection: cursor falls back to the clamped initial cursor.
    expect(snapshot.cursor).toBe(2);
    expect(snapshot.expandedCursor).toBeGreaterThanOrEqual(snapshot.cursor);

    // Selection set inside the editor is reflected on the next snapshot.
    setCollapsedSelection(editor, () => {
      const children = $paragraph().getChildren();
      const textNode = children.find((child) => child.getType() === "text");
      if (!textNode) throw new Error("expected text node");
      return { key: textNode.getKey(), offset: 3, type: "text" };
    });
    const moved = handle!.readSnapshot();
    expect(moved.cursor).toBe(4);
  });

  it("focus helpers bail out gracefully without a mounted root element", () => {
    const { editorRef, onChange } = renderEditor({ value: "hello" });
    const handle = editorRef.current;
    expect(handle).not.toBeNull();
    handle!.focus();
    handle!.focusAt(2);
    handle!.focusAtEnd();
    expect(onChange).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// Command key plugin
// ---------------------------------------------------------------------------

describe("ComposerCommandKeyPlugin", () => {
  it("delegates composer keys and consumes handled events", () => {
    const onCommandKeyDown = vi.fn(() => true);
    const { editor } = renderEditor({ value: "hello", onCommandKeyDown });

    const enter = keyEvent({ keyCode: 13 });
    expect(editor.dispatchCommand(KEY_ENTER_COMMAND, enter)).toBe(true);
    expect(onCommandKeyDown).toHaveBeenCalledWith("Enter", enter);
    expect(enter.preventDefault).toHaveBeenCalled();
    expect(enter.stopPropagation).toHaveBeenCalled();

    const tab = keyEvent();
    expect(editor.dispatchCommand(KEY_TAB_COMMAND, tab)).toBe(true);
    expect(onCommandKeyDown).toHaveBeenCalledWith("Tab", tab);

    const down = keyEvent();
    editor.dispatchCommand(KEY_ARROW_DOWN_COMMAND, down);
    expect(onCommandKeyDown).toHaveBeenCalledWith("ArrowDown", down);

    const up = keyEvent();
    editor.dispatchCommand(KEY_ARROW_UP_COMMAND, up);
    expect(onCommandKeyDown).toHaveBeenCalledWith("ArrowUp", up);
  });

  it("swallows Enter during IME composition without delegating", () => {
    const onCommandKeyDown = vi.fn(() => true);
    const { editor } = renderEditor({ value: "hello", onCommandKeyDown });

    const composing = keyEvent({ isComposing: true });
    expect(editor.dispatchCommand(KEY_ENTER_COMMAND, composing)).toBe(true);
    expect(composing.stopPropagation).toHaveBeenCalled();
    expect(onCommandKeyDown).not.toHaveBeenCalled();

    const keyCode229 = keyEvent({ keyCode: 229 });
    expect(editor.dispatchCommand(KEY_ENTER_COMMAND, keyCode229)).toBe(true);
    expect(onCommandKeyDown).not.toHaveBeenCalled();
  });

  it("does nothing when no command handler is provided", () => {
    const { editor } = renderEditor({ value: "hello" });
    const tab = keyEvent();
    expect(editor.dispatchCommand(KEY_TAB_COMMAND, tab)).toBe(false);
    expect(tab.preventDefault).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// Arrow navigation across inline tokens
// ---------------------------------------------------------------------------

describe("ComposerInlineTokenArrowPlugin", () => {
  it("steps left across an inline token boundary", () => {
    const { editor } = renderEditor({ value: `${MENTION_SOURCE} after` });

    setCollapsedSelection(editor, () => {
      const children = $paragraph().getChildren();
      const textNode = children.find((child) => child.getType() === "text");
      if (!textNode) throw new Error("expected text node");
      return { key: textNode.getKey(), offset: 0, type: "text" };
    });

    const event = keyEvent();
    expect(editor.dispatchCommand(KEY_ARROW_LEFT_COMMAND, event)).toBe(true);
    expect(event.preventDefault).toHaveBeenCalled();
    expect(readAnchor(editor)).toEqual({ type: "element", offset: 0 });
  });

  it("steps right across an inline token boundary", () => {
    const { editor } = renderEditor({ value: `${MENTION_SOURCE} after` });

    setCollapsedSelection(editor, () => ({
      key: $paragraph().getKey(),
      offset: 0,
      type: "element",
    }));

    const event = keyEvent();
    expect(editor.dispatchCommand(KEY_ARROW_RIGHT_COMMAND, event)).toBe(true);
    expect(readAnchor(editor)).toEqual({ type: "element", offset: 1 });
  });

  it("ignores arrows when the cursor is not adjacent to a token", () => {
    const { editor } = renderEditor({ value: `${MENTION_SOURCE} after` });

    setCollapsedSelection(editor, () => {
      const children = $paragraph().getChildren();
      const textNode = children.find((child) => child.getType() === "text");
      if (!textNode) throw new Error("expected text node");
      return { key: textNode.getKey(), offset: 3, type: "text" };
    });

    const left = keyEvent();
    expect(editor.dispatchCommand(KEY_ARROW_LEFT_COMMAND, left)).toBe(false);
    const right = keyEvent();
    expect(editor.dispatchCommand(KEY_ARROW_RIGHT_COMMAND, right)).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// Backspace over inline tokens
// ---------------------------------------------------------------------------

describe("ComposerInlineTokenBackspacePlugin", () => {
  it("removes the token preceding the caret at a text-node start", () => {
    const { editor, onChange } = renderEditor({ value: `${MENTION_SOURCE} after` });

    setCollapsedSelection(editor, () => {
      const children = $paragraph().getChildren();
      const textNode = children.find((child) => child.getType() === "text");
      if (!textNode) throw new Error("expected text node");
      return { key: textNode.getKey(), offset: 0, type: "text" };
    });
    onChange.mockClear();

    const event = keyEvent();
    expect(editor.dispatchCommand(KEY_BACKSPACE_COMMAND, event)).toBe(true);
    expect(event.preventDefault).toHaveBeenCalled();
    expect(readRootText(editor)).toBe(" after");
    expect(onChange).toHaveBeenCalled();
  });

  it("removes a terminal-context token and reports its id", () => {
    const { editor, onRemoveTerminalContext } = renderEditor({
      value: `${INLINE_TERMINAL_CONTEXT_PLACEHOLDER} tail`,
      terminalContexts: [terminalContext("ctx-del")],
    });

    setCollapsedSelection(editor, () => ({
      key: $paragraph().getKey(),
      offset: 1,
      type: "element",
    }));

    const event = keyEvent();
    expect(editor.dispatchCommand(KEY_BACKSPACE_COMMAND, event)).toBe(true);
    expect(onRemoveTerminalContext).toHaveBeenCalledWith("ctx-del");
    expect(readRootText(editor)).toBe(" tail");
  });

  it("leaves plain-text deletions to the default handler", () => {
    const { editor } = renderEditor({ value: `${MENTION_SOURCE} after` });

    setCollapsedSelection(editor, () => {
      const children = $paragraph().getChildren();
      const textNode = children.find((child) => child.getType() === "text");
      if (!textNode) throw new Error("expected text node");
      return { key: textNode.getKey(), offset: 4, type: "text" };
    });

    const event = keyEvent();
    // Our plugin declines mid-text deletions; the inline token must survive.
    editor.dispatchCommand(KEY_BACKSPACE_COMMAND, event);
    expect(readRootText(editor)).toContain(MENTION_SOURCE);
  });
});

// ---------------------------------------------------------------------------
// Selection normalization inside tokens
// ---------------------------------------------------------------------------

describe("ComposerInlineTokenSelectionNormalizePlugin", () => {
  it("re-anchors a selection that lands inside an inline token", async () => {
    const { editor } = renderEditor({ value: `${MENTION_SOURCE} after` });

    setDoctoredTokenSelection(editor, "composer-mention", 1);

    await flushMicrotasks();

    expect(readAnchor(editor)).toEqual({ type: "element", offset: 1 });
  });

  it("leaves token-start selections alone", async () => {
    const { editor } = renderEditor({ value: `${MENTION_SOURCE} after` });

    setDoctoredTokenSelection(editor, "composer-mention", 0);

    await flushMicrotasks();

    expect(readAnchor(editor)).toEqual({ type: "text", offset: 0 });
  });
});

// ---------------------------------------------------------------------------
// OnChange notifications
// ---------------------------------------------------------------------------

describe("ComposerPromptEditor onChange", () => {
  it("reports value, cursors, adjacency, and context ids after an edit", () => {
    const { editor, onChange } = renderEditor({
      value: `${MENTION_SOURCE} after`,
      cursor: 1,
    });
    onChange.mockClear();

    editor.update(
      () => {
        $paragraph().append($createTextNode("!"));
      },
      { discrete: true },
    );

    expect(onChange).toHaveBeenCalled();
    const lastCall = onChange.mock.calls.at(-1);
    expect(lastCall?.[0]).toBe(`${MENTION_SOURCE} after!`);
    expect(typeof lastCall?.[1]).toBe("number");
    expect(typeof lastCall?.[2]).toBe("number");
    expect(typeof lastCall?.[3]).toBe("boolean");
    expect(lastCall?.[4]).toEqual([]);
  });

  it("does not report a change when nothing moved", () => {
    const { editor, onChange } = renderEditor({ value: "static" });
    onChange.mockClear();

    // A no-op update leaves value, cursor, and contexts identical.
    commitUpdates(editor);

    expect(onChange).not.toHaveBeenCalled();
  });

  it("skips reporting when a re-set selection produces an identical snapshot", () => {
    const { editor, onChange } = renderEditor({ value: "abc" });

    const selectAtOne = () => {
      setCollapsedSelection(editor, () => {
        const text = $paragraph().getFirstChild();
        if (!text) throw new Error("expected text node");
        return { key: text.getKey(), offset: 1, type: "text" };
      });
    };

    selectAtOne();
    onChange.mockClear();
    // Same points again: the editor commits a dirty selection but the
    // derived snapshot is unchanged, so no change is emitted.
    selectAtOne();

    expect(onChange).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// Offset math across line breaks and stacked tokens
// ---------------------------------------------------------------------------

describe("ComposerPromptEditor offset math", () => {
  it("computes element-anchored snapshots across line breaks and tokens", () => {
    const value = `a\n${MENTION_SOURCE} mid ${INLINE_TERMINAL_CONTEXT_PLACEHOLDER} tail`;
    const { editor, editorRef } = renderEditor({
      value,
      terminalContexts: [terminalContext("ctx-math")],
    });

    // Point after the terminal token: children are
    // [text "a", linebreak, mention, text " mid ", terminal, text " tail"].
    setCollapsedSelection(editor, () => ({
      key: $paragraph().getKey(),
      offset: 5,
      type: "element",
    }));

    const snapshot = editorRef.current!.readSnapshot();
    expect(snapshot.cursor).toBe(9);
    expect(snapshot.expandedCursor).toBe(8 + MENTION_SOURCE.length);
    expect(snapshot.value).toBe(value);
  });

  it("navigates across stacked tokens and line breaks", () => {
    const m1 = serializeComposerFileLink("src/a.ts");
    const m2 = serializeComposerFileLink("src/b.ts");

    // Line break before the token region exercises the linebreak walk.
    const first = renderEditor({ value: `a\n${m1} x ${m2} b` });
    setCollapsedSelection(first.editor, () => {
      const children = $paragraph().getChildren();
      const tail = children[children.length - 1];
      if (!tail) throw new Error("expected trailing text node");
      return { key: tail.getKey(), offset: 0, type: "text" };
    });
    const leftEvent = keyEvent();
    expect(first.editor.dispatchCommand(KEY_ARROW_LEFT_COMMAND, leftEvent)).toBe(true);
    expect(readAnchor(first.editor)).toEqual({ type: "text", offset: 3 });

    // Two mentions separated by a space (tokens require whitespace
    // boundaries): stepping right across the second token resolves to an
    // element point one past it.
    const second = renderEditor({ value: `${m1} ${m2} b` });
    setCollapsedSelection(second.editor, () => ({
      key: $paragraph().getKey(),
      offset: 2,
      type: "element",
    }));
    const rightEvent = keyEvent();
    expect(second.editor.dispatchCommand(KEY_ARROW_RIGHT_COMMAND, rightEvent)).toBe(true);
    expect(readAnchor(second.editor)).toEqual({ type: "element", offset: 3 });
  });

  it("guards arrow navigation without a selection or at the edges", () => {
    const { editor } = renderEditor({ value: "ab" });

    // No selection at all.
    expect(editor.dispatchCommand(KEY_ARROW_LEFT_COMMAND, keyEvent())).toBe(false);
    expect(editor.dispatchCommand(KEY_ARROW_RIGHT_COMMAND, keyEvent())).toBe(false);

    // At the very start, left cannot move further.
    setCollapsedSelection(editor, () => ({
      key: $paragraph().getKey(),
      offset: 0,
      type: "element",
    }));
    expect(editor.dispatchCommand(KEY_ARROW_LEFT_COMMAND, keyEvent())).toBe(false);

    // At the very end, right cannot move further.
    setCollapsedSelection(editor, () => {
      const text = $paragraph().getFirstChild();
      if (!text) throw new Error("expected text node");
      return { key: text.getKey(), offset: 2, type: "text" };
    });
    expect(editor.dispatchCommand(KEY_ARROW_RIGHT_COMMAND, keyEvent())).toBe(false);
  });

  it("removes a leading skill token directly under the caret", () => {
    // Skill tokens need a trailing whitespace boundary to tokenize.
    const { editor } = renderEditor({ value: "$refactor ", skills });

    setDoctoredTokenSelection(editor, "composer-skill", 1);

    const event = keyEvent();
    expect(editor.dispatchCommand(KEY_BACKSPACE_COMMAND, event)).toBe(true);
    expect(event.preventDefault).toHaveBeenCalled();
    expect(readRootText(editor)).toBe(" ");
  });

  it("guards backspace without a selection and around non-token neighbors", () => {
    const noSelection = renderEditor({ value: "ab" });
    expect(noSelection.editor.dispatchCommand(KEY_BACKSPACE_COMMAND, keyEvent())).toBe(false);

    // Caret at the start of the second line: the previous sibling is a line
    // break, not an inline token, so the plugin defers.
    const multiline = renderEditor({ value: "a\nb" });
    setCollapsedSelection(multiline.editor, () => {
      const children = $paragraph().getChildren();
      const tail = children[children.length - 1];
      if (!tail) throw new Error("expected trailing text node");
      return { key: tail.getKey(), offset: 0, type: "text" };
    });
    multiline.editor.dispatchCommand(KEY_BACKSPACE_COMMAND, keyEvent());
    expect(readRootText(multiline.editor)).toContain("a");

    // Element point at offset zero has no preceding child to remove.
    const atStart = renderEditor({ value: "ab" });
    setCollapsedSelection(atStart.editor, () => ({
      key: $paragraph().getKey(),
      offset: 0,
      type: "element",
    }));
    atStart.editor.dispatchCommand(KEY_BACKSPACE_COMMAND, keyEvent());
    expect(readRootText(atStart.editor)).toBe("ab");
  });
});

// ---------------------------------------------------------------------------
// Focus with a mounted root element
// ---------------------------------------------------------------------------

describe("ComposerPromptEditor focus with a root element", () => {
  it("focuses the root and reports the clamped cursor", () => {
    const { editor, editorRef, onChange } = renderEditor({ value: "hello world" });
    const fakeRoot = createFakeRootElement();
    (editor as unknown as { getRootElement: () => HTMLElement | null }).getRootElement = () =>
      fakeRoot.element;

    onChange.mockClear();
    editorRef.current!.focusAt(3);
    expect(fakeRoot.focus).toHaveBeenCalledWith({ preventScroll: true });
    expect(onChange).toHaveBeenCalledWith("hello world", 3, 3, false, []);

    onChange.mockClear();
    editorRef.current!.focusAtEnd();
    expect(onChange).toHaveBeenCalledWith("hello world", 11, 11, false, []);

    onChange.mockClear();
    editorRef.current!.focus();
    expect(onChange).toHaveBeenCalledWith("hello world", 11, 11, false, []);
  });
});

// ---------------------------------------------------------------------------
// Controlled value reconciliation
// ---------------------------------------------------------------------------

describe("ComposerPromptEditor controlled updates", () => {
  it("rewrites the editor when the controlled value diverges from the snapshot", async () => {
    const { editor, onChange } = renderEditor({ value: "seed value" });

    // Editor drifts away from the controlled prop...
    editor.update(
      () => {
        $paragraph().append($createTextNode("X"));
      },
      { discrete: true },
    );
    expect(readRootText(editor)).toBe("seed valueX");

    onChange.mockClear();
    // ...then the layout effect re-fires (a controlled re-render) and must
    // restore the prop value while suppressing the resulting change event.
    reflushExecutedEffects();
    await flushMicrotasks();

    expect(readRootText(editor)).toBe("seed value");
    expect(onChange).not.toHaveBeenCalled();
  });

  it("rethrows editor errors through the composer onError handler", () => {
    const { editor } = renderEditor({ value: "boom" });
    expect(() =>
      editor.update(
        () => {
          throw new Error("editor exploded");
        },
        { discrete: true },
      ),
    ).toThrow("editor exploded");
  });
});

// ---------------------------------------------------------------------------
// Surround-selection typing behavior
// ---------------------------------------------------------------------------

interface SurroundKeyEventInit {
  key?: string;
  code?: string;
  defaultPrevented?: boolean;
  isComposing?: boolean;
  metaKey?: boolean;
  ctrlKey?: boolean;
}

function surroundKeyEvent(init: SurroundKeyEventInit = {}): KeyboardEvent {
  return {
    key: init.key ?? "(",
    code: init.code ?? "",
    defaultPrevented: init.defaultPrevented ?? false,
    isComposing: init.isComposing ?? false,
    metaKey: init.metaKey ?? false,
    ctrlKey: init.ctrlKey ?? false,
  } as unknown as KeyboardEvent;
}

interface SurroundInputEventInit {
  inputType: string;
  data?: string | null;
}

function surroundInputEvent(init: SurroundInputEventInit) {
  return {
    inputType: init.inputType,
    data: init.data,
    preventDefault: vi.fn(),
    stopPropagation: vi.fn(),
    stopImmediatePropagation: vi.fn(),
  };
}

describe("ComposerSurroundSelectionPlugin", () => {
  function renderWithRoot(value: string) {
    const rendered = renderEditor({ value });
    const fakeRoot = createFakeRootElement();
    for (const listener of rendered.rootListeners) {
      listener(fakeRoot.element, null);
    }
    return { ...rendered, fakeRoot };
  }

  function selectFirstTextRange(editor: LexicalEditor, start: number, end: number) {
    setRangeSelection(editor, () => {
      const text = $paragraph().getFirstChild();
      if (!text) throw new Error("expected text node");
      return {
        anchor: { key: text.getKey(), offset: start, type: "text" },
        focus: { key: text.getKey(), offset: end, type: "text" },
      };
    });
  }

  it("wraps the selected text when a surround symbol is typed", () => {
    const { editor, fakeRoot } = renderWithRoot("pick me");
    selectFirstTextRange(editor, 0, 4);

    fakeRoot.dispatch("keydown", surroundKeyEvent({ key: "(" }));
    const beforeInput = surroundInputEvent({ inputType: "insertText", data: "(" });
    fakeRoot.dispatch("beforeinput", beforeInput);

    expect(beforeInput.preventDefault).toHaveBeenCalled();
    expect(beforeInput.stopImmediatePropagation).toHaveBeenCalled();
    expect(readRootText(editor)).toBe("(pick) me");
  });

  it("falls back to the live selection when no keydown snapshot exists", () => {
    const { editor, fakeRoot } = renderWithRoot("pick me");
    selectFirstTextRange(editor, 5, 7);

    const beforeInput = surroundInputEvent({ inputType: "insertText", data: "[" });
    fakeRoot.dispatch("beforeinput", beforeInput);

    expect(readRootText(editor)).toBe("pick [me]");
  });

  it("ignores non-surround characters and modifier chords", () => {
    const { editor, fakeRoot } = renderWithRoot("pick me");
    selectFirstTextRange(editor, 0, 4);

    // Modifier chords clear any pending snapshot.
    fakeRoot.dispatch("keydown", surroundKeyEvent({ key: "(", metaKey: true }));

    // Plain characters do not surround; the default insertion continues.
    fakeRoot.dispatch("keydown", surroundKeyEvent({ key: "z" }));
    const plain = surroundInputEvent({ inputType: "insertText", data: "z" });
    fakeRoot.dispatch("beforeinput", plain);
    expect(plain.preventDefault).not.toHaveBeenCalled();
    expect(readRootText(editor)).toBe("pick me");

    // Collapsed selections never trigger surround handling.
    setCollapsedSelection(editor, () => {
      const text = $paragraph().getFirstChild();
      if (!text) throw new Error("expected text node");
      return { key: text.getKey(), offset: 2, type: "text" };
    });
    fakeRoot.dispatch("keydown", surroundKeyEvent({ key: "(" }));
    const collapsed = surroundInputEvent({ inputType: "insertText", data: "(" });
    fakeRoot.dispatch("beforeinput", collapsed);
    expect(readRootText(editor)).toBe("pick me");

    // Multi-character and non-string beforeinput payloads reset the snapshot.
    selectFirstTextRange(editor, 0, 4);
    fakeRoot.dispatch("keydown", surroundKeyEvent({ key: "(" }));
    fakeRoot.dispatch("beforeinput", surroundInputEvent({ inputType: "insertText", data: "ab" }));
    fakeRoot.dispatch("beforeinput", surroundInputEvent({ inputType: "insertReplacementText" }));
    expect(readRootText(editor)).toBe("pick me");
  });

  it("skips selections that touch inline tokens", () => {
    const rendered = renderEditor({ value: `${MENTION_SOURCE} after` });
    const fakeRoot = createFakeRootElement();
    for (const listener of rendered.rootListeners) {
      listener(fakeRoot.element, null);
    }
    const { editor } = rendered;

    // Anchor before the mention, focus inside the trailing text: the range
    // covers the token node.
    setRangeSelection(editor, () => {
      const children = $paragraph().getChildren();
      const tail = children[children.length - 1];
      if (!tail) throw new Error("expected trailing text node");
      return {
        anchor: { key: $paragraph().getKey(), offset: 0, type: "element" },
        focus: { key: tail.getKey(), offset: 3, type: "text" },
      };
    });

    fakeRoot.dispatch("keydown", surroundKeyEvent({ key: "(" }));
    const beforeInput = surroundInputEvent({ inputType: "insertText", data: "(" });
    fakeRoot.dispatch("beforeinput", beforeInput);
    expect(beforeInput.preventDefault).not.toHaveBeenCalled();
    expect(readRootText(editor)).toBe(`${MENTION_SOURCE} after`);
  });

  it("applies the dead-key backtick surround after composition resolves", async () => {
    const { editor, fakeRoot } = renderWithRoot("pick me");
    selectFirstTextRange(editor, 0, 4);

    fakeRoot.dispatch("keydown", surroundKeyEvent({ key: "`" }));
    // Dead-key composition inserts the backtick as composition text.
    fakeRoot.dispatch(
      "beforeinput",
      surroundInputEvent({ inputType: "insertCompositionText", data: "`" }),
    );
    // Dead-key state survives further dead/space keydowns.
    fakeRoot.dispatch("keydown", surroundKeyEvent({ key: "Dead" }));

    // The browser resolves the composition by replacing the selection with a
    // literal backtick; emulate that model change.
    editor.update(
      () => {
        const text = $paragraph().getFirstChild();
        if (!text) throw new Error("expected text node");
        const selection = $createRangeSelection();
        selection.anchor.set(text.getKey(), 0, "text");
        selection.focus.set(text.getKey(), 4, "text");
        $setSelection(selection);
        selection.insertText("`");
      },
      { discrete: true },
    );
    expect(readRootText(editor)).toBe("` me");

    fakeRoot.dispatch("input", surroundInputEvent({ inputType: "insertText", data: "`" }));
    await flushMicrotasks();

    expect(readRootText(editor)).toBe("`pick` me");
  });

  it("abandons the dead-key surround when composition resolves differently", async () => {
    const { editor, fakeRoot } = renderWithRoot("pick me");
    selectFirstTextRange(editor, 0, 4);

    fakeRoot.dispatch("keydown", surroundKeyEvent({ key: "`" }));
    fakeRoot.dispatch(
      "beforeinput",
      surroundInputEvent({ inputType: "insertCompositionText", data: "`" }),
    );

    // Composition resolved to something other than a bare backtick.
    editor.update(
      () => {
        const text = $paragraph().getFirstChild();
        if (!text) throw new Error("expected text node");
        const selection = $createRangeSelection();
        selection.anchor.set(text.getKey(), 0, "text");
        selection.focus.set(text.getKey(), 4, "text");
        $setSelection(selection);
        selection.insertText("è");
      },
      { discrete: true },
    );

    fakeRoot.dispatch("compositionend", {});
    await flushMicrotasks();

    expect(readRootText(editor)).toBe("è me");
  });
});
