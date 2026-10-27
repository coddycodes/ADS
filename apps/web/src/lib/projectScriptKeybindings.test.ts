import { MAX_KEYBINDING_VALUE_LENGTH, type KeybindingCommand } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import { commandForProjectScript } from "../projectScripts";
import {
  decodeProjectScriptKeybindingRule,
  keybindingValueForCommand,
  PROJECT_SCRIPT_KEYBINDING_INVALID_MESSAGE,
} from "./projectScriptKeybindings";

describe("projectScriptKeybindings", () => {
  it("decodes and trims valid keybinding rules", () => {
    const rule = decodeProjectScriptKeybindingRule({
      keybinding: "  mod+k  ",
      command: commandForProjectScript("lint"),
    });

    expect(rule).toEqual({
      key: "mod+k",
      command: "script.lint.run",
    });
  });

  it("returns null when keybinding is empty", () => {
    expect(
      decodeProjectScriptKeybindingRule({
        keybinding: "   ",
        command: commandForProjectScript("lint"),
      }),
    ).toBeNull();
  });

  it("rejects invalid keybinding values", () => {
    expect(() =>
      decodeProjectScriptKeybindingRule({
        keybinding: "k".repeat(MAX_KEYBINDING_VALUE_LENGTH + 1),
        command: commandForProjectScript("lint"),
      }),
    ).toThrowError(PROJECT_SCRIPT_KEYBINDING_INVALID_MESSAGE);
  });

  it("rejects invalid commands", () => {
    expect(() =>
      decodeProjectScriptKeybindingRule({
        keybinding: "mod+k",
        command: "script.BAD.run" as KeybindingCommand,
      }),
    ).toThrowError(PROJECT_SCRIPT_KEYBINDING_INVALID_MESSAGE);
  });

  it("reads latest matching keybinding value for a command", () => {
    const command = commandForProjectScript("test");
    const value = keybindingValueForCommand(
      [
        {
          command,
          shortcut: {
            key: "escape",
            metaKey: false,
            ctrlKey: false,
            shiftKey: false,
            altKey: false,
            modKey: true,
          },
        },
        {
          command,
          shortcut: {
            key: "k",
            metaKey: false,
            ctrlKey: false,
            shiftKey: true,
            altKey: false,
            modKey: true,
          },
        },
      ],
      command,
    );

    expect(value).toBe("mod+shift+k");
  });

  it("skips sparse and unrelated bindings and formats every modifier and named key", () => {
    const command = commandForProjectScript("format");
    const unrelated = commandForProjectScript("lint");
    const sparse = [
      undefined,
      {
        command: unrelated,
        shortcut: {
          key: "x",
          metaKey: false,
          ctrlKey: false,
          shiftKey: false,
          altKey: false,
          modKey: false,
        },
      },
      {
        command,
        shortcut: {
          key: " ",
          metaKey: true,
          ctrlKey: true,
          shiftKey: true,
          altKey: true,
          modKey: true,
        },
      },
    ] as never;

    expect(keybindingValueForCommand(sparse, command)).toBe("mod+ctrl+meta+alt+shift+space");
    expect(keybindingValueForCommand(sparse, unrelated)).toBe("x");
    expect(keybindingValueForCommand([], command)).toBeNull();

    expect(
      keybindingValueForCommand(
        [
          {
            command,
            shortcut: {
              key: "escape",
              metaKey: false,
              ctrlKey: false,
              shiftKey: false,
              altKey: false,
              modKey: false,
            },
          },
        ],
        command,
      ),
    ).toBe("esc");
  });
});
