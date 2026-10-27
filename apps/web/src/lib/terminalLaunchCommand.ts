import {
  TerminalLaunchCommand,
  type TerminalLaunchCommand as TerminalLaunchCommandValue,
} from "@coddy/contracts";
import * as Schema from "effect/Schema";

const decodeTerminalLaunchCommandOption = Schema.decodeUnknownOption(TerminalLaunchCommand);

export function decodeTerminalLaunchCommand(value: unknown): TerminalLaunchCommandValue | null {
  const decoded = decodeTerminalLaunchCommandOption(value);
  return decoded._tag === "Some" ? decoded.value : null;
}

export function decodePersistedTerminalLaunchCommand(
  value: unknown,
): TerminalLaunchCommandValue | null {
  const decoded = decodeTerminalLaunchCommand(value);
  if (decoded !== null) return decoded;
  if (value === null || typeof value !== "object" || Array.isArray(value)) return null;
  if (!Object.hasOwn(value, "activity")) return null;
  const { activity: _malformedActivity, ...command } = value as Record<string, unknown>;
  return decodeTerminalLaunchCommand(command);
}
