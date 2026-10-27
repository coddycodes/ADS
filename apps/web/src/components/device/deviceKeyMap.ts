import type { DeviceInputEvent, DeviceKey } from "@coddy/contracts";

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

type KeyEventModifiers = Pick<KeyboardEvent, "key" | "ctrlKey" | "metaKey" | "altKey"> & {
  readonly shiftKey?: boolean;
};

/**
 * Whether a key press belongs to the app, not the device: a chord with Ctrl, Cmd or Alt, or
 * Shift+Tab, which leaves the screen.
 */
export function isAppShortcut(event: KeyEventModifiers): boolean {
  return (
    event.ctrlKey ||
    event.metaKey ||
    event.altKey ||
    (event.key === "Tab" && event.shiftKey === true)
  );
}

/**
 * The device input for a key on the focused screen, or `null` to leave it to the app.
 *
 * On key down, printable characters become text and editing and arrow keys become key presses.
 * A shortcut ({@link isAppShortcut}) is not captured, so app shortcuts and leaving the screen
 * with the keyboard keep working.
 *
 * A key-up for a mapped key is always a key release, whatever modifiers are held now: Ctrl may
 * join after the press was forwarded, and a dropped release leaves the key held on the device.
 * The input queue sends it only for a key it holds, so the release of a shortcut press that
 * was never forwarded goes nowhere.
 */
export function keyboardEventToDeviceInput(
  event: KeyEventModifiers,
  action: "down" | "up" = "down",
): DeviceInputEvent | null {
  const key = KEYS[event.key];
  if (action === "up") return key === undefined ? null : { type: "key", key, action };
  if (isAppShortcut(event)) return null;
  if (key !== undefined) return { type: "key", key, action };
  // One code point: a character, not a named key such as "Shift" or "F5".
  if (isSingleCodePoint(event.key)) return { type: "text", text: event.key };
  return null;
}

function isSingleCodePoint(value: string): boolean {
  if (value.length === 0 || value.length > 2) return false;
  const codePoint = value.codePointAt(0)!;
  return value.length === (codePoint > 0xffff ? 2 : 1);
}

/** Printable ASCII plus Return and Tab: what the server types on an iOS simulator's US layout. */
const US_TYPEABLE = /^[\x20-\x7E\n\r\t]*$/;

/**
 * Whether an iOS simulator gets `text` typed as keys. Any other text is pasted through the
 * simulator's pasteboard, where iOS may ask to Allow Paste. The server's `is_us_typeable`
 * (`apps/server/src/device/input.rs`) is the same set, outside which Android refuses text;
 * the two tests share their vectors.
 */
export function isUsTypeable(text: string): boolean {
  return US_TYPEABLE.test(text);
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
