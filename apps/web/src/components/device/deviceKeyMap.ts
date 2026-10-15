import type { DeviceInputEvent, DeviceKey } from "@bibcode/contracts";

/** The longest text one input event carries, in UTF-16 units. */
export const MAX_DEVICE_TEXT_LENGTH = 1_000;

const KEYS: Readonly<Record<string, DeviceKey>> = {
  Backspace: "backspace",
  Delete: "delete",
  Enter: "enter",
  Tab: "tab",
  ArrowUp: "arrowUp",
  ArrowDown: "arrowDown",
  ArrowLeft: "arrowLeft",
  ArrowRight: "arrowRight",
};

/**
 * The device input for a key pressed on the focused screen, or `null` to leave it to the app.
 * Printable characters become text on key down; editing and arrow keys become key presses.
 * Escape, Shift+Tab and anything with Ctrl, Cmd or Alt are not captured, so app shortcuts
 * and leaving the screen with the keyboard keep working.
 */
export function keyboardEventToDeviceInput(
  event: Pick<KeyboardEvent, "key" | "ctrlKey" | "metaKey" | "altKey"> & {
    readonly shiftKey?: boolean;
  },
  action: "down" | "up" = "down",
): DeviceInputEvent | null {
  if (event.ctrlKey || event.metaKey || event.altKey) return null;
  if (event.key === "Tab" && event.shiftKey === true) return null;
  const key = KEYS[event.key];
  if (key !== undefined) return { type: "key", key, action };
  // One code point: a character, not a named key such as "Shift" or "F5".
  if (action === "down" && isSingleCodePoint(event.key)) return { type: "text", text: event.key };
  return null;
}

function isSingleCodePoint(value: string): boolean {
  if (value.length === 0 || value.length > 2) return false;
  const codePoint = value.codePointAt(0)!;
  return value.length === (codePoint > 0xffff ? 2 : 1);
}

/** Splits text into parts the server accepts, never between the halves of a surrogate pair. */
export function splitDeviceText(text: string, maxLength = MAX_DEVICE_TEXT_LENGTH): string[] {
  const parts: string[] = [];
  let start = 0;
  while (start < text.length) {
    let end = Math.min(start + maxLength, text.length);
    if (end < text.length && isHighSurrogate(text.charCodeAt(end - 1)) && end - 1 > start) {
      end -= 1;
    }
    parts.push(text.slice(start, end));
    start = end;
  }
  return parts;
}

function isHighSurrogate(code: number): boolean {
  return code >= 0xd800 && code <= 0xdbff;
}
