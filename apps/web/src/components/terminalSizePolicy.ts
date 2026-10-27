import type { TerminalSize } from "@coddy/contracts";

export type TerminalDimensions = Pick<TerminalSize, "cols" | "rows">;
export type TerminalSizeTrigger = "attach" | "focus" | "pointer" | "keypress" | "layout" | "fit";

interface TerminalSizePolicy {
  readonly sizeClaim: string;
  readonly applied: TerminalSize | null;
  readonly fitted: TerminalDimensions | null;
  readonly claimInFlight: boolean;
}

export function terminalDimensionsEqual(
  left: TerminalDimensions | null | undefined,
  right: TerminalDimensions | null | undefined,
): boolean {
  return left?.cols === right?.cols && left?.rows === right?.rows;
}

export function terminalSizesEqual(
  left: TerminalSize | null | undefined,
  right: TerminalSize | null | undefined,
): boolean {
  return terminalDimensionsEqual(left, right) && left?.sizeClaim === right?.sizeClaim;
}

export function hasForeignTerminalSizeOwner(
  applied: TerminalSize | null,
  sizeClaim: string,
): boolean {
  return applied?.sizeClaim != null && applied.sizeClaim !== sizeClaim;
}

export function shouldShowTerminalSizeNotice(input: TerminalSizePolicy): boolean {
  return (
    !input.claimInFlight &&
    input.applied !== null &&
    input.fitted !== null &&
    hasForeignTerminalSizeOwner(input.applied, input.sizeClaim) &&
    !terminalDimensionsEqual(input.applied, input.fitted)
  );
}

export function shouldClaimTerminalSize(
  input: TerminalSizePolicy & {
    readonly trigger: TerminalSizeTrigger;
    readonly documentFocused: boolean;
  },
): boolean {
  if (input.claimInFlight || input.fitted === null || input.applied === null) return false;
  if (input.applied.sizeClaim === null) return true;
  const ownsSize = input.applied.sizeClaim === input.sizeClaim;
  // Matching owner geometry needs no resize or claim request.
  if (ownsSize && terminalDimensionsEqual(input.applied, input.fitted)) return false;
  switch (input.trigger) {
    case "attach":
      return input.documentFocused;
    case "layout":
      return ownsSize;
    case "fit":
    case "focus":
    case "pointer":
    case "keypress":
      return true;
  }
}
