import { describe, expect, it } from "vite-plus/test";
import { terminalInputKey } from "@coddy/client-runtime/state/terminal";
import {
  proposeTerminalDimensions,
  readTerminalFittedSize,
  registerTerminalSizeReader,
} from "./terminalSizing";

describe("terminal fitting", () => {
  it("uses the protocol bounds for fitted sizes and ignores unmeasurable geometry", () => {
    expect(
      proposeTerminalDimensions({ proposeDimensions: () => ({ cols: 3000, rows: 900 }) }),
    ).toEqual({ cols: 1000, rows: 500 });
    expect(proposeTerminalDimensions({ proposeDimensions: () => ({ cols: 0, rows: 0 }) })).toEqual({
      cols: 1,
      rows: 1,
    });
    expect(proposeTerminalDimensions({ proposeDimensions: () => undefined })).toBeNull();
    expect(
      proposeTerminalDimensions({ proposeDimensions: () => ({ cols: NaN, rows: 20 }) }),
    ).toBeNull();
    expect(
      proposeTerminalDimensions({
        proposeDimensions: () => {
          throw new Error("detached");
        },
      }),
    ).toBeNull();
  });

  it("reads only mounted renderer sizes on demand and does not remove a replacement reader", () => {
    const key = terminalInputKey("env", "thread", "term");
    expect(readTerminalFittedSize("env", "thread", "term")).toBeNull();
    let dimensions = { cols: 91, rows: 42 };
    const unregister = registerTerminalSizeReader(key, () => dimensions);
    expect(readTerminalFittedSize("env", "thread", "term")).toEqual({ cols: 91, rows: 42 });
    dimensions = { cols: 151, rows: 50 };
    expect(readTerminalFittedSize("env", "thread", "term")).toEqual({ cols: 151, rows: 50 });
    const replace = registerTerminalSizeReader(key, () => ({ cols: 80, rows: 24 }));
    unregister();
    expect(readTerminalFittedSize("env", "thread", "term")).toEqual({ cols: 80, rows: 24 });
    replace();
    expect(readTerminalFittedSize("env", "thread", "term")).toBeNull();
  });
});
