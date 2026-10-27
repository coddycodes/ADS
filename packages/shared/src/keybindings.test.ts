import type { KeybindingRule } from "@coddy/contracts";
import { MAX_KEYBINDINGS_COUNT, MAX_WHEN_EXPRESSION_DEPTH } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";
import {
  compileResolvedKeybindingRule,
  compileResolvedKeybindingsConfig,
  DEFAULT_KEYBINDINGS,
  DEFAULT_RESOLVED_KEYBINDINGS,
  parseKeybindingShortcut,
  parseKeybindingWhenExpression,
} from "./keybindings.ts";

describe("parseKeybindingShortcut", () => {
  it("parses a plain key without modifiers", () => {
    expect(parseKeybindingShortcut("b")).toEqual({
      key: "b",
      metaKey: false,
      ctrlKey: false,
      shiftKey: false,
      altKey: false,
      modKey: false,
    });
  });

  it("parses every modifier alias", () => {
    expect(parseKeybindingShortcut("cmd+ctrl+shift+alt+mod+b")).toEqual({
      key: "b",
      metaKey: true,
      ctrlKey: true,
      shiftKey: true,
      altKey: true,
      modKey: true,
    });
    expect(parseKeybindingShortcut("meta+control+option+k")).toEqual({
      key: "k",
      metaKey: true,
      ctrlKey: true,
      shiftKey: false,
      altKey: true,
      modKey: false,
    });
  });

  it("normalizes repeated platform modifier aliases without changing the key", () => {
    expect(parseKeybindingShortcut("cmd+meta+ctrl+control+alt+option+mod+mod+k")).toEqual({
      key: "k",
      metaKey: true,
      ctrlKey: true,
      shiftKey: false,
      altKey: true,
      modKey: true,
    });
  });

  it("lowercases and trims whitespace around tokens", () => {
    expect(parseKeybindingShortcut(" MOD + B ")).toMatchObject({ key: "b", modKey: true });
  });

  it("normalizes the space token to a literal space", () => {
    expect(parseKeybindingShortcut("mod+space")).toMatchObject({ key: " ", modKey: true });
  });

  it("normalizes the esc token to escape", () => {
    expect(parseKeybindingShortcut("esc")).toMatchObject({ key: "escape" });
  });

  it("treats a trailing plus as the literal plus key", () => {
    expect(parseKeybindingShortcut("mod++")).toMatchObject({ key: "+", modKey: true });
  });

  it("returns the plus key when the whole shortcut is a plus", () => {
    expect(parseKeybindingShortcut("+")).toMatchObject({ key: "+" });
  });

  it("rejects an interior empty token", () => {
    expect(parseKeybindingShortcut("mod++b")).toBeNull();
  });

  it("rejects shortcuts with two non-modifier keys", () => {
    expect(parseKeybindingShortcut("a+b")).toBeNull();
  });

  it("rejects shortcuts that contain only modifiers", () => {
    expect(parseKeybindingShortcut("mod+shift")).toBeNull();
  });
});

describe("parseKeybindingWhenExpression", () => {
  it("parses a bare identifier", () => {
    expect(parseKeybindingWhenExpression("terminalFocus")).toEqual({
      type: "identifier",
      name: "terminalFocus",
    });
  });

  it("parses identifiers containing dots and dashes", () => {
    expect(parseKeybindingWhenExpression("editor.read-only")).toEqual({
      type: "identifier",
      name: "editor.read-only",
    });
  });

  it("parses negation", () => {
    expect(parseKeybindingWhenExpression("!terminalFocus")).toEqual({
      type: "not",
      node: { type: "identifier", name: "terminalFocus" },
    });
  });

  it("collapses stacked negations into nested not nodes", () => {
    expect(parseKeybindingWhenExpression("!!a")).toEqual({
      type: "not",
      node: { type: "not", node: { type: "identifier", name: "a" } },
    });
  });

  it("parses conjunction with correct precedence over disjunction", () => {
    expect(parseKeybindingWhenExpression("a && b || c")).toEqual({
      type: "or",
      left: {
        type: "and",
        left: { type: "identifier", name: "a" },
        right: { type: "identifier", name: "b" },
      },
      right: { type: "identifier", name: "c" },
    });
  });

  it("honors explicit parentheses over default precedence", () => {
    expect(parseKeybindingWhenExpression("(a || b) && c")).toEqual({
      type: "and",
      left: {
        type: "or",
        left: { type: "identifier", name: "a" },
        right: { type: "identifier", name: "b" },
      },
      right: { type: "identifier", name: "c" },
    });
  });

  it("ignores whitespace between tokens", () => {
    expect(parseKeybindingWhenExpression("  a   &&   b  ")).toEqual({
      type: "and",
      left: { type: "identifier", name: "a" },
      right: { type: "identifier", name: "b" },
    });
  });

  it("returns null for an empty expression", () => {
    expect(parseKeybindingWhenExpression("")).toBeNull();
    expect(parseKeybindingWhenExpression("   ")).toBeNull();
  });

  it("returns null for an invalid character", () => {
    expect(parseKeybindingWhenExpression("a @ b")).toBeNull();
    expect(parseKeybindingWhenExpression("@")).toBeNull();
  });

  it("returns null when a leading operand is a stray operator", () => {
    expect(parseKeybindingWhenExpression(")")).toBeNull();
  });

  it("returns null when the conjunction right operand is missing", () => {
    expect(parseKeybindingWhenExpression("a &&")).toBeNull();
  });

  it("returns null when the disjunction right operand is missing", () => {
    expect(parseKeybindingWhenExpression("a ||")).toBeNull();
  });

  it("returns null when a parenthesis is never closed", () => {
    expect(parseKeybindingWhenExpression("(a")).toBeNull();
  });

  it("returns null when the token after a group is not a closing paren", () => {
    expect(parseKeybindingWhenExpression("(a a")).toBeNull();
  });

  it("returns null when there are trailing tokens after a complete expression", () => {
    expect(parseKeybindingWhenExpression("a b")).toBeNull();
  });

  it("returns null when the negation has no operand", () => {
    expect(parseKeybindingWhenExpression("!")).toBeNull();
  });

  it("returns null when parentheses nest deeper than the max depth", () => {
    const depth = MAX_WHEN_EXPRESSION_DEPTH + 1;
    const expression = "(".repeat(depth) + "a" + ")".repeat(depth);
    expect(parseKeybindingWhenExpression(expression)).toBeNull();
  });

  it("returns null when negations stack deeper than the max depth", () => {
    const expression = "!".repeat(MAX_WHEN_EXPRESSION_DEPTH + 1) + "a";
    expect(parseKeybindingWhenExpression(expression)).toBeNull();
  });
});

describe("compileResolvedKeybindingRule", () => {
  it("compiles a rule without a when clause", () => {
    const rule: KeybindingRule = { key: "mod+b", command: "sidebar.toggle" };
    expect(compileResolvedKeybindingRule(rule)).toEqual({
      command: "sidebar.toggle",
      shortcut: parseKeybindingShortcut("mod+b"),
    });
  });

  it("compiles a rule with a valid when clause", () => {
    const rule: KeybindingRule = {
      key: "mod+d",
      command: "terminal.split",
      when: "terminalFocus",
    };
    expect(compileResolvedKeybindingRule(rule)).toEqual({
      command: "terminal.split",
      shortcut: parseKeybindingShortcut("mod+d"),
      whenAst: { type: "identifier", name: "terminalFocus" },
    });
  });

  it("returns null when the key cannot be parsed", () => {
    const rule: KeybindingRule = { key: "a+b", command: "sidebar.toggle" };
    expect(compileResolvedKeybindingRule(rule)).toBeNull();
  });

  it("returns null when the when clause cannot be parsed", () => {
    const rule: KeybindingRule = {
      key: "mod+b",
      command: "sidebar.toggle",
      when: "@invalid",
    };
    expect(compileResolvedKeybindingRule(rule)).toBeNull();
  });
});

describe("compileResolvedKeybindingsConfig", () => {
  it("compiles valid rules and drops the invalid ones", () => {
    const config: ReadonlyArray<KeybindingRule> = [
      { key: "mod+b", command: "sidebar.toggle" },
      { key: "a+b", command: "terminal.newCenter" },
      { key: "mod+d", command: "terminal.split", when: "@bad" },
      { key: "mod+j", command: "terminal.newCenter" },
    ];
    const compiled = compileResolvedKeybindingsConfig(config);
    expect(compiled.map((rule) => rule.command)).toEqual(["sidebar.toggle", "terminal.newCenter"]);
  });

  it("returns an empty config when every rule is invalid", () => {
    const config: ReadonlyArray<KeybindingRule> = [{ key: "a+b", command: "sidebar.toggle" }];
    expect(compileResolvedKeybindingsConfig(config)).toEqual([]);
  });

  it("preserves conflicting shortcuts in declaration order", () => {
    const config: ReadonlyArray<KeybindingRule> = [
      { key: "mod+b", command: "sidebar.toggle" },
      { key: "mod+b", command: "terminal.newCenter" },
    ];

    expect(compileResolvedKeybindingsConfig(config).map((rule) => rule.command)).toEqual([
      "sidebar.toggle",
      "terminal.newCenter",
    ]);
  });

  it("retains the most recently declared rules at the configured limit", () => {
    const config: ReadonlyArray<KeybindingRule> = Array.from(
      { length: MAX_KEYBINDINGS_COUNT + 2 },
      (_, index) => ({ key: `key${index}`, command: "sidebar.toggle" }),
    );

    const compiled = compileResolvedKeybindingsConfig(config);

    expect(compiled).toHaveLength(MAX_KEYBINDINGS_COUNT);
    expect(compiled[0]?.shortcut.key).toBe("key2");
    expect(compiled.at(-1)?.shortcut.key).toBe(`key${MAX_KEYBINDINGS_COUNT + 1}`);
  });
});

describe("DEFAULT_KEYBINDINGS", () => {
  it("steers a queued message with mod+shift+enter while editing", () => {
    expect(DEFAULT_KEYBINDINGS).toContainEqual({
      key: "mod+shift+enter",
      command: "thread.steerQueuedMessage",
      when: "editableFocus",
    });
  });

  it("opens a new center terminal from mod+j", () => {
    expect(DEFAULT_KEYBINDINGS).toContainEqual({
      key: "mod+j",
      command: "terminal.newCenter",
    });
  });

  it("includes the thread jump and model picker jump bindings", () => {
    expect(DEFAULT_KEYBINDINGS.some((rule) => rule.command === "thread.jump.1")).toBe(true);
    expect(
      DEFAULT_KEYBINDINGS.some(
        (rule) => rule.command === "modelPicker.jump.1" && rule.when === "modelPickerOpen",
      ),
    ).toBe(true);
  });

  it("compiles every default binding into the resolved defaults", () => {
    expect(DEFAULT_RESOLVED_KEYBINDINGS.length).toBe(DEFAULT_KEYBINDINGS.length);
    expect(DEFAULT_RESOLVED_KEYBINDINGS.every((rule) => rule.shortcut.key.length > 0)).toBe(true);
  });
});
