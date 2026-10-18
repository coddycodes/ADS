import { useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";

import { cn } from "~/lib/utils";

/** Rows rendered beyond each edge of the viewport. */
const OVERSCAN_ROWS = 8;
/** Rows rendered before the viewport is measured (server render, no layout). */
const UNMEASURED_ROWS = 40;

/** Top offset of every row plus the total height as the last entry. */
export function rowOffsets(heights: readonly number[]): number[] {
  const offsets = [0];
  for (const height of heights) offsets.push(offsets[offsets.length - 1]! + height);
  return offsets;
}

/** Index of the last row that starts at or above `position`. */
function rowAt(offsets: readonly number[], position: number): number {
  let low = 0;
  let high = offsets.length - 2;
  while (low < high) {
    const middle = Math.ceil((low + high) / 2);
    if (offsets[middle]! <= position) low = middle;
    else high = middle - 1;
  }
  return Math.max(low, 0);
}

/**
 * The half-open row range to render for a scroll position: the rows the
 * viewport shows plus an overscan margin on each side.
 */
export function visibleRowRange(
  offsets: readonly number[],
  scrollTop: number,
  viewportHeight: number,
): { start: number; end: number } {
  const count = offsets.length - 1;
  if (viewportHeight <= 0) return { start: 0, end: Math.min(count, UNMEASURED_ROWS) };
  const first = rowAt(offsets, scrollTop);
  // A row that starts exactly at the bottom edge is not in view.
  const last = rowAt(offsets, scrollTop + viewportHeight - 1);
  return {
    start: Math.max(0, first - OVERSCAN_ROWS),
    end: Math.min(count, last + 1 + OVERSCAN_ROWS),
  };
}

interface WindowedListProps<T> {
  items: readonly T[];
  /** Every row's fixed height in pixels; it must match the rendered row. */
  itemHeight: (item: T) => number;
  itemKey: (item: T) => string;
  renderItem: (item: T, index: number) => ReactNode;
  className?: string;
  style?: CSSProperties;
  role?: string;
  "aria-label"?: string;
  /** The opaque surface under the rows, for LCD text on Linux WebKit. */
  textSurface?: "background" | "card" | "sidebar" | "popover";
}

/**
 * A scroll container that mounts only the rows near its viewport (#119).
 * Rows stay in document order, so Tab and screen-reader reading order follow
 * the screen; skipped rows are plain spacers, not positioned layers. The row
 * holding focus stays mounted while it scrolls out of the window.
 */
export function WindowedList<T>(props: WindowedListProps<T>) {
  const { items, itemHeight, itemKey, renderItem } = props;
  const scrollerRef = useRef<HTMLDivElement>(null);
  const [viewport, setViewport] = useState({ scrollTop: 0, height: 0 });
  const [focusedKey, setFocusedKey] = useState<string | null>(null);
  const offsets = rowOffsets(items.map(itemHeight));
  const total = offsets[offsets.length - 1]!;
  const { start, end } = visibleRowRange(offsets, viewport.scrollTop, viewport.height);
  const indices = Array.from({ length: end - start }, (_, index) => start + index);
  const focusedIndex =
    focusedKey === null ? -1 : items.findIndex((item) => itemKey(item) === focusedKey);
  if (focusedIndex !== -1 && (focusedIndex < start || focusedIndex >= end)) {
    if (focusedIndex < start) indices.unshift(focusedIndex);
    else indices.push(focusedIndex);
  }
  // Each rendered row follows a spacer for the rows skipped above it.
  const rendered: ReactNode[] = [];
  let cursor = 0;
  for (const index of indices) {
    const item = items[index]!;
    const key = itemKey(item);
    if (offsets[index]! > cursor) {
      rendered.push(<div key={`gap:${key}`} style={{ height: offsets[index]! - cursor }} />);
    }
    rendered.push(
      <div key={key} data-row-key={key} style={{ height: itemHeight(item) }}>
        {renderItem(item, index)}
      </div>,
    );
    cursor = offsets[index + 1]!;
  }

  useLayoutEffect(() => {
    const scroller = scrollerRef.current;
    if (!scroller) return;
    const measure = () => {
      const height = scroller.getBoundingClientRect().height;
      const scrollTop = scroller.scrollTop;
      setViewport((current) =>
        current.height === height && current.scrollTop === scrollTop
          ? current
          : { scrollTop, height },
      );
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(scroller);
    return () => observer.disconnect();
  }, []);

  return (
    <div
      ref={scrollerRef}
      // A Tab stop like the ScrollArea viewport it replaces, so the keyboard
      // can scroll the list; WebKit does not focus scrollers on its own.
      tabIndex={0}
      className={cn(
        "overflow-y-auto outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring",
        props.className,
      )}
      style={props.style}
      role={props.role}
      aria-label={props["aria-label"]}
      onScroll={(event) => {
        // Re-render only when the window moves; the stored offset may lag by
        // less than a row, which the overscan absorbs.
        const scrollTop = event.currentTarget.scrollTop;
        const next = visibleRowRange(offsets, scrollTop, viewport.height);
        if (next.start === start && next.end === end) return;
        setViewport((current) => ({ ...current, scrollTop }));
      }}
    >
      <div
        data-text-surface={props.textSurface}
        style={{ height: total }}
        onFocus={(event) => {
          const row = (event.target as Element).closest("[data-row-key]");
          setFocusedKey(row?.getAttribute("data-row-key") ?? null);
        }}
        onBlur={(event) => {
          if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
            setFocusedKey(null);
          }
        }}
      >
        {rendered}
      </div>
    </div>
  );
}
