import { describe, expect, it } from "vite-plus/test";

import { isUsTypeable, keyboardEventToDeviceInput, splitDeviceText } from "./deviceKeyMap";

const press = (
  key: string,
  modifiers: Partial<Record<"ctrlKey" | "metaKey" | "altKey" | "shiftKey", boolean>> = {},
) => ({
  key,
  ctrlKey: false,
  metaKey: false,
  altKey: false,
  ...modifiers,
});

describe("keyboardEventToDeviceInput", () => {
  it("sends printable characters as text", () => {
    expect(keyboardEventToDeviceInput(press("a"))).toEqual({ type: "text", text: "a" });
    expect(keyboardEventToDeviceInput(press("A", { shiftKey: true }))).toEqual({
      type: "text",
      text: "A",
    });
    expect(keyboardEventToDeviceInput(press("😀"))).toEqual({ type: "text", text: "😀" });
  });

  it("sends editing and arrow keys as key presses", () => {
    expect(keyboardEventToDeviceInput(press("Backspace"))).toEqual({
      type: "key",
      key: "backspace",
      action: "down",
    });
    expect(keyboardEventToDeviceInput(press("ArrowLeft"), "up")).toEqual({
      type: "key",
      key: "arrowLeft",
      action: "up",
    });
    expect(keyboardEventToDeviceInput(press("Enter"))).toEqual({
      type: "key",
      key: "enter",
      action: "down",
    });
  });

  it("leaves Escape, shortcuts and Shift+Tab to the app", () => {
    expect(keyboardEventToDeviceInput(press("Escape"))).toBeNull();
    expect(keyboardEventToDeviceInput(press("c", { metaKey: true }))).toBeNull();
    expect(keyboardEventToDeviceInput(press("v", { ctrlKey: true }))).toBeNull();
    expect(keyboardEventToDeviceInput(press("e", { altKey: true }))).toBeNull();
    expect(keyboardEventToDeviceInput(press("Tab", { shiftKey: true }))).toBeNull();
    expect(keyboardEventToDeviceInput(press("Shift"))).toBeNull();
    expect(keyboardEventToDeviceInput(press("F5"))).toBeNull();
  });

  it("leaves a chord with Ctrl, Cmd or Alt, and Shift+Tab, to the app on key down", () => {
    expect(keyboardEventToDeviceInput(press("Backspace", { ctrlKey: true }))).toBeNull();
    expect(keyboardEventToDeviceInput(press("Backspace", { metaKey: true }))).toBeNull();
    expect(keyboardEventToDeviceInput(press("ArrowLeft", { altKey: true }))).toBeNull();
    expect(keyboardEventToDeviceInput(press("Tab", { shiftKey: true }), "down")).toBeNull();
  });

  it("releases a key whatever modifiers are held when it comes up", () => {
    // The press was forwarded before the modifier joined; dropping the release would leave the
    // key held on the device.
    for (const modifier of ["ctrlKey", "metaKey", "altKey"] as const) {
      expect(keyboardEventToDeviceInput(press("Backspace", { [modifier]: true }), "up")).toEqual({
        type: "key",
        key: "backspace",
        action: "up",
      });
    }
    expect(keyboardEventToDeviceInput(press("Tab", { shiftKey: true }), "up")).toEqual({
      type: "key",
      key: "tab",
      action: "up",
    });
  });

  it("still releases nothing for keys that were never forwarded", () => {
    expect(keyboardEventToDeviceInput(press("Escape"), "up")).toBeNull();
    expect(keyboardEventToDeviceInput(press("c", { metaKey: true }), "up")).toBeNull();
    expect(keyboardEventToDeviceInput(press("Shift"), "up")).toBeNull();
  });

  it("sends no text when a printable key is released", () => {
    expect(keyboardEventToDeviceInput(press("a"), "up")).toBeNull();
  });
});

describe("splitDeviceText", () => {
  it("keeps short text whole and drops empty text", () => {
    expect(splitDeviceText("hello")).toEqual(["hello"]);
    expect(splitDeviceText("")).toEqual([]);
  });

  it("splits long text into parts of at most the limit", () => {
    const text = "x".repeat(2_500);
    expect(splitDeviceText(text).map((part) => part.length)).toEqual([1_000, 1_000, 500]);
  });

  it("never splits a surrogate pair", () => {
    const text = `${"x".repeat(999)}😀y`;
    const parts = splitDeviceText(text);
    expect(parts).toEqual(["x".repeat(999), "😀y"]);
    expect(parts.join("")).toBe(text);
  });
});

// The same vectors as the server's `us_typeable_text_is_printable_ascii_with_return_and_tab`
// (`apps/server/src/device/input.rs`), so the two sets cannot drift apart unnoticed.
describe("isUsTypeable", () => {
  it("accepts every printable ASCII character, Return and Tab", () => {
    const printable = Array.from({ length: 0x7f - 0x20 }, (_, index) =>
      String.fromCharCode(0x20 + index),
    ).join("");
    expect(isUsTypeable(printable)).toBe(true);
    expect(isUsTypeable("line one\nline two\r\n\tindented")).toBe(true);
    expect(isUsTypeable("")).toBe(true);
  });

  it("refuses accented letters, CJK, emoji and other control characters", () => {
    for (const refused of [
      "héllo",
      "é",
      "日本",
      "\u{1f600}",
      "a\u00a0b",
      "\u007f",
      "\u0000",
      "\u001b",
      "\u0008",
      "\u2028",
    ]) {
      expect(isUsTypeable(refused), JSON.stringify(refused)).toBe(false);
    }
  });
});
