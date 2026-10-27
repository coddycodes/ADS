import { assert, it } from "@effect/vitest";
import * as Schema from "effect/Schema";
import * as Effect from "effect/Effect";

import {
  KeybindingsConfigError,
  KeybindingsConfig,
  KeybindingCommand,
  KeybindingRule,
  KeybindingWhenNode,
  ResolvedKeybindingRule,
  ResolvedKeybindingsConfig,
} from "./keybindings.ts";
import { expectDecodeFailure, expectEncodeFailure } from "./test/schemaAssertions.ts";

const decode = <S extends Schema.Top>(
  schema: S,
  input: unknown,
): Effect.Effect<Schema.Schema.Type<S>, Schema.SchemaError, never> =>
  Schema.decodeUnknownEffect(schema as never)(input) as Effect.Effect<
    Schema.Schema.Type<S>,
    Schema.SchemaError,
    never
  >;

const decodeResolvedRule = Schema.decodeUnknownEffect(ResolvedKeybindingRule as never);
const decodeWhenNode = Schema.decodeUnknownSync(KeybindingWhenNode);
const encodeWhenNode = Schema.encodeSync(KeybindingWhenNode);
const decodeConfigError = Schema.decodeUnknownSync(KeybindingsConfigError);
const encodeConfigError = Schema.encodeSync(KeybindingsConfigError);
const decodeKeybindingCommand = Schema.decodeUnknownSync(KeybindingCommand);

const EXPECTED_STATIC_KEYBINDING_COMMANDS = [
  "sidebar.toggle",
  "terminal.newCenter",
  "terminal.split",
  "terminal.splitVertical",
  "terminal.new",
  "terminal.close",
  "rightPanel.toggle",
  "diff.toggle",
  "preview.toggle",
  "preview.refresh",
  "preview.focusUrl",
  "preview.zoomIn",
  "preview.zoomOut",
  "preview.resetZoom",
  "commandPalette.toggle",
  "chat.new",
  "chat.newLocal",
  "editor.openFavorite",
  "thread.steerQueuedMessage",
  "modelPicker.toggle",
  "modelPicker.jump.1",
  "modelPicker.jump.2",
  "modelPicker.jump.3",
  "modelPicker.jump.4",
  "modelPicker.jump.5",
  "modelPicker.jump.6",
  "modelPicker.jump.7",
  "modelPicker.jump.8",
  "modelPicker.jump.9",
  "thread.previous",
  "thread.next",
  "thread.jump.1",
  "thread.jump.2",
  "thread.jump.3",
  "thread.jump.4",
  "thread.jump.5",
  "thread.jump.6",
  "thread.jump.7",
  "thread.jump.8",
  "thread.jump.9",
] as const;

it("anchors every static KeybindingCommand literal", () => {
  assert.deepStrictEqual(
    EXPECTED_STATIC_KEYBINDING_COMMANDS.map((command) => decodeKeybindingCommand(command)),
    [...EXPECTED_STATIC_KEYBINDING_COMMANDS],
  );
});

it("accepts the center-terminal command", () => {
  assert.strictEqual(decodeKeybindingCommand("terminal.newCenter"), "terminal.newCenter");
});

it.effect("parses keybinding rules", () =>
  Effect.gen(function* () {
    const parsed = yield* decode(KeybindingRule, {
      key: "mod+j",
      command: "terminal.newCenter",
    });
    assert.strictEqual(parsed.command, "terminal.newCenter");

    const parsedSidebarToggle = yield* decode(KeybindingRule, {
      key: "mod+b",
      command: "sidebar.toggle",
    });
    assert.strictEqual(parsedSidebarToggle.command, "sidebar.toggle");

    const parsedRightPanelToggle = yield* decode(KeybindingRule, {
      key: "mod+alt+b",
      command: "rightPanel.toggle",
    });
    assert.strictEqual(parsedRightPanelToggle.command, "rightPanel.toggle");

    const parsedClose = yield* decode(KeybindingRule, {
      key: "mod+w",
      command: "terminal.close",
    });
    assert.strictEqual(parsedClose.command, "terminal.close");

    const parsedDiffToggle = yield* decode(KeybindingRule, {
      key: "mod+d",
      command: "diff.toggle",
    });
    assert.strictEqual(parsedDiffToggle.command, "diff.toggle");

    const parsedCommandPalette = yield* decode(KeybindingRule, {
      key: "mod+k",
      command: "commandPalette.toggle",
    });
    assert.strictEqual(parsedCommandPalette.command, "commandPalette.toggle");

    const parsedLocal = yield* decode(KeybindingRule, {
      key: "mod+shift+n",
      command: "chat.newLocal",
    });
    assert.strictEqual(parsedLocal.command, "chat.newLocal");

    const parsedModelPickerToggle = yield* decode(KeybindingRule, {
      key: "mod+shift+m",
      command: "modelPicker.toggle",
    });
    assert.strictEqual(parsedModelPickerToggle.command, "modelPicker.toggle");

    const parsedModelPickerJump = yield* decode(KeybindingRule, {
      key: "mod+1",
      command: "modelPicker.jump.1",
    });
    assert.strictEqual(parsedModelPickerJump.command, "modelPicker.jump.1");

    const parsedThreadPrevious = yield* decode(KeybindingRule, {
      key: "mod+shift+[",
      command: "thread.previous",
    });
    assert.strictEqual(parsedThreadPrevious.command, "thread.previous");
  }),
);

it("reports invalid command values at the command path on decode and encode", () => {
  const invalid = { key: "mod+j", command: "script.Test.run" };
  const expected = {
    rootTag: "Composite" as const,
    paths: [["command"]],
    containsTag: "AnyOf" as const,
  };
  expectDecodeFailure(KeybindingRule, invalid, expected);
  expectEncodeFailure(KeybindingRule, invalid, expected);
});

it.effect("accepts dynamic script run commands", () =>
  Effect.gen(function* () {
    const parsed = yield* decode(KeybindingRule, {
      key: "mod+r",
      command: "script.setup.run",
    });
    assert.strictEqual(parsed.command, "script.setup.run");
  }),
);

it.effect("parses keybindings array payload", () =>
  Effect.gen(function* () {
    const parsed = yield* decode(KeybindingsConfig, [
      { key: "mod+j", command: "terminal.newCenter" },
      { key: "mod+d", command: "terminal.split", when: "terminalFocus" },
      { key: "mod+shift+d", command: "terminal.splitVertical", when: "terminalFocus" },
    ]);
    assert.lengthOf(parsed, 3);
  }),
);

it.effect("parses resolved keybinding rules", () =>
  Effect.gen(function* () {
    const parsed = yield* decode(ResolvedKeybindingRule, {
      command: "terminal.split",
      shortcut: {
        key: "d",
        metaKey: false,
        ctrlKey: false,
        shiftKey: false,
        altKey: false,
        modKey: true,
      },
      whenAst: {
        type: "and",
        left: { type: "identifier", name: "terminalOpen" },
        right: {
          type: "not",
          node: { type: "identifier", name: "terminalFocus" },
        },
      },
    });
    assert.strictEqual(parsed.shortcut.key, "d");
  }),
);

it.effect("parses resolved keybindings arrays", () =>
  Effect.gen(function* () {
    const parsed = yield* decode(ResolvedKeybindingsConfig, [
      {
        command: "terminal.newCenter",
        shortcut: {
          key: "j",
          metaKey: false,
          ctrlKey: false,
          shiftKey: false,
          altKey: false,
          modKey: true,
        },
      },
      {
        command: "thread.jump.3",
        shortcut: {
          key: "3",
          metaKey: false,
          ctrlKey: false,
          shiftKey: false,
          altKey: false,
          modKey: true,
        },
      },
    ]);
    assert.lengthOf(parsed, 2);
  }),
);

it.effect("drops unknown fields in resolved keybinding rules", () =>
  decodeResolvedRule({
    command: "terminal.newCenter",
    shortcut: {
      key: "j",
      metaKey: false,
      ctrlKey: false,
      shiftKey: false,
      altKey: false,
      modKey: true,
    },
    key: "mod+j",
  }).pipe(
    Effect.map((parsed) => {
      const view = parsed as Record<string, unknown>;
      assert.strictEqual("key" in view, false);
      assert.strictEqual(view.command, "terminal.newCenter");
    }),
  ),
);

it("round-trips every keybinding expression node alternative", () => {
  const nodes = [
    { type: "identifier", name: "terminalOpen" },
    { type: "not", node: { type: "identifier", name: "terminalFocus" } },
    {
      type: "and",
      left: { type: "identifier", name: "terminalOpen" },
      right: { type: "identifier", name: "terminalFocus" },
    },
    {
      type: "or",
      left: { type: "identifier", name: "editorFocus" },
      right: { type: "identifier", name: "terminalFocus" },
    },
  ] as const;

  for (const node of nodes) {
    const decoded = decodeWhenNode(node);
    assert.deepStrictEqual(encodeWhenNode(decoded), node);
  }
});

it("reports invalid nested expression nodes on decode and encode", () => {
  const invalid = {
    type: "not",
    node: { type: "identifier", name: "" },
  };
  const expected = {
    rootTag: "AnyOf" as const,
    paths: [["node", "name"]],
    containsTag: "InvalidValue" as const,
  };
  expectDecodeFailure(KeybindingWhenNode, invalid, expected);
  expectEncodeFailure(KeybindingWhenNode, invalid, expected);
});

it("constructs and round-trips keybindings config errors", () => {
  const error = new KeybindingsConfigError({
    configPath: "/repo/.coddy/studio/keybindings.json",
    detail: "invalid command",
  });

  assert.strictEqual(
    error.message,
    "Unable to parse keybindings config at /repo/.coddy/studio/keybindings.json: invalid command",
  );
  const encoded = encodeConfigError(error);
  assert.strictEqual(decodeConfigError(encoded)._tag, "KeybindingsConfigParseError");
});
