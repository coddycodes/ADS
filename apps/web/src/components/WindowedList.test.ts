import { describe, expect, it } from "vite-plus/test";

import { rowOffsets, visibleRowRange } from "./WindowedList";

describe("visibleRowRange", () => {
  const offsets = rowOffsets(Array.from({ length: 1_000 }, () => 10));

  it("places every row at the sum of the heights above it", () => {
    expect(rowOffsets([5, 10, 20])).toEqual([0, 5, 15, 35]);
  });

  it("renders the rows in view plus an overscan margin", () => {
    // Rows 50..59 fill the viewport; eight more render on each side.
    expect(visibleRowRange(offsets, 500, 100)).toEqual({ start: 42, end: 68 });
  });

  it("clamps the range to the list", () => {
    expect(visibleRowRange(offsets, 0, 100)).toEqual({ start: 0, end: 18 });
    expect(visibleRowRange(offsets, 9_950, 100)).toEqual({ start: 987, end: 1_000 });
  });

  it("renders a first screenful before the viewport is measured", () => {
    expect(visibleRowRange(offsets, 0, 0)).toEqual({ start: 0, end: 40 });
    expect(visibleRowRange(rowOffsets([]), 0, 0)).toEqual({ start: 0, end: 0 });
    expect(visibleRowRange(rowOffsets([]), 0, 300)).toEqual({ start: 0, end: 0 });
  });
});
