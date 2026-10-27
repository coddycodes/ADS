import type { ContextMenuEntry, ContextMenuItem } from "@coddy/contracts";

import {
  consumeKeyboardContextMenuOpenedAt,
  KEYBOARD_CONTEXT_MENU_ECHO_MS,
} from "./contextMenuKeyboard";

const SVG_NS = "http://www.w3.org/2000/svg";

let nextMenuElementId = 0;
/** A document-unique id for the label and reason spans an item references. */
function menuElementId(part: "label" | "reason"): string {
  nextMenuElementId += 1;
  return `coddy-studio-context-menu-${part}-${nextMenuElementId}`;
}

// Inline Lucide-style icon paths (stroke-based, viewBox 0 0 24 24, strokeWidth 2).
const ICON_PATHS: Record<string, ReadonlyArray<{ tag: string; attrs: Record<string, string> }>> = {
  pencil: [
    {
      tag: "path",
      attrs: {
        d: "M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z",
      },
    },
    { tag: "path", attrs: { d: "m15 5 4 4" } },
  ],
  copy: [
    { tag: "rect", attrs: { width: "14", height: "14", x: "8", y: "8", rx: "2", ry: "2" } },
    { tag: "path", attrs: { d: "M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" } },
  ],
  "folder-tree": [
    {
      tag: "path",
      attrs: {
        d: "M20 10a1 1 0 0 0 1-1V6a1 1 0 0 0-1-1h-2.5a1 1 0 0 1-.8-.4l-.9-1.2A1 1 0 0 0 15 3h-2a1 1 0 0 0-1 1v5a1 1 0 0 0 1 1Z",
      },
    },
    {
      tag: "path",
      attrs: {
        d: "M20 21a1 1 0 0 0 1-1v-3a1 1 0 0 0-1-1h-2.9a1 1 0 0 1-.88-.55l-.42-.85a1 1 0 0 0-.92-.6H13a1 1 0 0 0-1 1v5a1 1 0 0 0 1 1Z",
      },
    },
    { tag: "path", attrs: { d: "M3 5a2 2 0 0 0 2 2h3" } },
    { tag: "path", attrs: { d: "M3 3v13a2 2 0 0 0 2 2h3" } },
  ],
  trash: [
    { tag: "path", attrs: { d: "M3 6h18" } },
    { tag: "path", attrs: { d: "M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6" } },
    { tag: "path", attrs: { d: "M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" } },
    { tag: "line", attrs: { x1: "10", x2: "10", y1: "11", y2: "17" } },
    { tag: "line", attrs: { x1: "14", x2: "14", y1: "11", y2: "17" } },
  ],
};

function createIconElement(name: string, tone: "neutral" | "destructive"): SVGSVGElement | null {
  const paths = ICON_PATHS[name];
  if (!paths) {
    return null;
  }
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("xmlns", SVG_NS);
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("fill", "none");
  svg.setAttribute("stroke", "currentColor");
  svg.setAttribute("stroke-width", "2");
  svg.setAttribute("stroke-linecap", "round");
  svg.setAttribute("stroke-linejoin", "round");
  svg.setAttribute(
    "class",
    tone === "destructive" ? "size-3.5 shrink-0" : "size-3.5 shrink-0 text-muted-foreground",
  );
  for (const node of paths) {
    const child = document.createElementNS(SVG_NS, node.tag);
    for (const [key, value] of Object.entries(node.attrs)) {
      child.setAttribute(key, value);
    }
    svg.appendChild(child);
  }
  return svg;
}

function clampMenuPosition(menu: HTMLDivElement, preferredLeft: number, preferredTop: number) {
  const rect = menu.getBoundingClientRect();
  const left = Math.min(
    Math.max(4, preferredLeft),
    Math.max(4, window.innerWidth - rect.width - 4),
  );
  const top = Math.min(
    Math.max(4, preferredTop),
    Math.max(4, window.innerHeight - rect.height - 4),
  );
  menu.style.left = `${left}px`;
  menu.style.top = `${top}px`;
}

function isNodeWithinMenuStack(target: EventTarget | null, menuStack: readonly HTMLDivElement[]) {
  if (typeof Node !== "undefined" && target instanceof Node) {
    return menuStack.some((menu) => menu.contains(target));
  }
  if (!target || typeof target !== "object") {
    return false;
  }

  let current: unknown = target;
  while (current && typeof current === "object") {
    if (menuStack.includes(current as HTMLDivElement)) {
      return true;
    }
    current = (current as { parent?: unknown }).parent;
  }
  return false;
}

/**
 * Drops leading and trailing separators and collapses runs. The fallback shows
 * headers and empty submenus as they come, so this is its only filtering.
 */
export function normalizeContextMenuEntries<T extends string>(
  entries: readonly ContextMenuEntry<T>[],
): ContextMenuEntry<T>[] {
  const normalized: ContextMenuEntry<T>[] = [];
  for (const entry of entries) {
    if ("separator" in entry) {
      const previous = normalized.at(-1);
      if (previous === undefined || "separator" in previous) {
        continue;
      }
    }
    normalized.push(entry);
  }
  const last = normalized.at(-1);
  if (last !== undefined && "separator" in last) {
    normalized.pop();
  }
  return normalized;
}

interface RenderedMenuItem<T extends string> {
  readonly element: HTMLButtonElement;
  readonly item: ContextMenuItem<T>;
  readonly enabled: boolean;
  readonly hasChildren: boolean;
}

function focusElement(element: HTMLElement | null | undefined): void {
  if (element && typeof element.focus === "function") {
    element.focus({ preventScroll: true });
    element.scrollIntoView?.({ block: "nearest" });
  }
}

function readFocusedElement(): HTMLElement | null {
  if (typeof HTMLElement === "undefined" || typeof document === "undefined") {
    return null;
  }
  const active = document.activeElement;
  return active instanceof HTMLElement ? active : null;
}

/**
 * Imperative DOM-based context menu for browsers and the Windows desktop
 * webview. Supports nested submenus, separators and full keyboard control, and
 * resolves with the chosen leaf item id. Disabled items stay reachable with
 * the arrow keys, Home and End, but Enter, Space and clicks never choose them
 * or open their submenus.
 */
export function showContextMenuFallback<T extends string>(
  items: readonly ContextMenuEntry<T>[],
  position?: { x: number; y: number },
): Promise<T | null> {
  return new Promise<T | null>((resolve) => {
    const menuStack: HTMLDivElement[] = [];
    const renderedItemsByLevel: RenderedMenuItem<T>[][] = [];
    // openParents[level] is the row at `level` whose submenu is open at level + 1.
    const openParents: RenderedMenuItem<T>[] = [];
    const returnFocusTo = readFocusedElement();
    let isDisposed = false;
    let canDismissFromPointer = false;
    const keyboardOpenedAt = consumeKeyboardContextMenuOpenedAt();
    const isKeyboardEcho = () => {
      const elapsed = performance.now() - keyboardOpenedAt;
      return elapsed >= 0 && elapsed < KEYBOARD_CONTEXT_MENU_ECHO_MS;
    };

    const cleanup = (result: T | null, options: { readonly restoreFocus: boolean }) => {
      if (isDisposed) {
        return;
      }
      isDisposed = true;
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("contextmenu", onContextMenu, true);
      for (const menu of menuStack) {
        menu.remove();
      }
      if (options.restoreFocus) {
        focusElement(returnFocusTo);
      }
      resolve(result);
    };

    // Every item, disabled ones included, takes part in the roving focus.
    const levelRecords = (level: number): RenderedMenuItem<T>[] =>
      renderedItemsByLevel[level] ?? [];

    /** A menu opens on its first enabled item, or its first item if none is enabled. */
    const focusOpeningItem = (level: number) => {
      const records = levelRecords(level);
      focusElement((records.find((record) => record.enabled) ?? records[0])?.element);
    };

    const focusBoundary = (level: number, boundary: "first" | "last") => {
      const records = levelRecords(level);
      focusElement((boundary === "first" ? records[0] : records.at(-1))?.element);
    };

    const focusedPosition = (): {
      readonly level: number;
      readonly record: RenderedMenuItem<T>;
    } | null => {
      const active = document.activeElement;
      for (let level = renderedItemsByLevel.length - 1; level >= 0; level -= 1) {
        const record = renderedItemsByLevel[level]?.find(
          (candidate) => candidate.element === active,
        );
        if (record) {
          return { level, record };
        }
      }
      return null;
    };

    const moveFocus = (step: 1 | -1) => {
      const position = focusedPosition();
      const level = position?.level ?? renderedItemsByLevel.length - 1;
      const records = levelRecords(level);
      if (records.length === 0) {
        return;
      }
      const currentIndex = position ? records.indexOf(position.record) : -1;
      const nextIndex =
        currentIndex === -1
          ? step === 1
            ? 0
            : records.length - 1
          : (currentIndex + step + records.length) % records.length;
      focusElement(records[nextIndex]?.element);
    };

    const onKeyDown = (event: KeyboardEvent) => {
      switch (event.key) {
        case "Escape":
        case "Tab": {
          event.preventDefault();
          event.stopPropagation();
          cleanup(null, { restoreFocus: true });
          return;
        }
        case "ArrowDown":
        case "ArrowUp": {
          event.preventDefault();
          event.stopPropagation();
          moveFocus(event.key === "ArrowDown" ? 1 : -1);
          return;
        }
        case "Home":
        case "End": {
          event.preventDefault();
          event.stopPropagation();
          const level = focusedPosition()?.level ?? renderedItemsByLevel.length - 1;
          focusBoundary(level, event.key === "Home" ? "first" : "last");
          return;
        }
        case "ArrowRight": {
          const position = focusedPosition();
          if (!position?.record.enabled || !position.record.hasChildren) {
            return;
          }
          event.preventDefault();
          event.stopPropagation();
          openSubmenu(position.record, position.level, true);
          return;
        }
        case "ArrowLeft": {
          const position = focusedPosition();
          if (!position || position.level === 0) {
            return;
          }
          event.preventDefault();
          event.stopPropagation();
          const parent = openParents[position.level - 1];
          closeMenusFromLevel(position.level);
          focusElement(parent?.element);
          return;
        }
        case "Enter":
        case " ": {
          const position = focusedPosition();
          if (!position) {
            return;
          }
          event.preventDefault();
          event.stopPropagation();
          if (!position.record.enabled) {
            // Focusable so it can explain itself, never chosen or opened.
            return;
          }
          if (position.record.hasChildren) {
            openSubmenu(position.record, position.level, true);
          } else {
            cleanup(position.record.item.id, { restoreFocus: true });
          }
          return;
        }
        default:
          return;
      }
    };

    const onPointerDown = (event: PointerEvent) => {
      if (isKeyboardEcho() && !isNodeWithinMenuStack(event.target, menuStack)) {
        event.preventDefault();
        event.stopPropagation();
        return;
      }
      if (!canDismissFromPointer || isNodeWithinMenuStack(event.target, menuStack)) {
        return;
      }
      cleanup(null, { restoreFocus: false });
    };

    const onContextMenu = (event: MouseEvent) => {
      if (isKeyboardEcho()) {
        event.preventDefault();
        event.stopPropagation();
        return;
      }
      if (!canDismissFromPointer || isNodeWithinMenuStack(event.target, menuStack)) {
        return;
      }
      event.preventDefault();
      cleanup(null, { restoreFocus: false });
    };

    const closeMenusFromLevel = (level: number) => {
      while (menuStack.length > level) {
        menuStack.pop()?.remove();
      }
      if (renderedItemsByLevel.length > level) {
        renderedItemsByLevel.length = level;
      }
      const firstStaleParent = Math.max(level - 1, 0);
      for (let index = openParents.length - 1; index >= firstStaleParent; index -= 1) {
        openParents[index]?.element.setAttribute("aria-expanded", "false");
      }
      if (openParents.length > firstStaleParent) {
        openParents.length = firstStaleParent;
      }
    };

    const openSubmenu = (record: RenderedMenuItem<T>, level: number, focusFirst: boolean) => {
      const children = record.item.children;
      if (!children) {
        return;
      }
      const rect = record.element.getBoundingClientRect();
      // Opens to the right of its row, or to the left when that would overflow.
      openMenu(children, rect.right + 4, rect.top, level + 1, (width) => rect.left - width - 4);
      openParents[level] = record;
      record.element.setAttribute("aria-expanded", "true");
      if (focusFirst) {
        focusOpeningItem(level + 1);
      }
    };

    const openMenu = (
      entries: readonly ContextMenuEntry<T>[],
      preferredLeft: number,
      preferredTop: number,
      level: number,
      leftWhenOverflowing?: (menuWidth: number) => number,
    ) => {
      closeMenusFromLevel(level);

      const menu = document.createElement("div");
      menu.setAttribute("role", "menu");
      menu.setAttribute("aria-orientation", "vertical");
      menu.tabIndex = -1;
      menu.className =
        "fixed z-[10000] min-w-32 max-w-sm overflow-hidden rounded-lg border border-border bg-popover bg-clip-padding text-popover-foreground shadow-lg/5 outline-none";
      menu.style.cssText =
        "position:fixed;z-index:10000;min-width:8rem;max-width:24rem;overflow:hidden;border-radius:var(--radius-lg);border:1px solid var(--border);background:var(--popover);background-clip:padding-box;color:var(--popover-foreground);box-shadow:0 10px 15px -3px rgb(0 0 0 / 0.05),0 4px 6px -4px rgb(0 0 0 / 0.05);outline:none;pointer-events:auto;";
      menu.style.left = `${preferredLeft}px`;
      menu.style.top = `${preferredTop}px`;
      menu.dataset.level = String(level);

      const inner = document.createElement("div");
      inner.className =
        "max-h-[min(24rem,70vh)] min-w-0 max-w-sm overflow-y-auto overflow-x-hidden p-1";
      inner.style.cssText =
        "max-height:min(24rem,70vh);min-width:0;max-width:24rem;overflow-x:hidden;overflow-y:auto;padding:0.25rem;";

      const records: RenderedMenuItem<T>[] = [];
      // Clears every other row's highlight, so a level shows one active row.
      const unhighlightByButton = new Map<HTMLButtonElement, () => void>();
      for (const entry of normalizeContextMenuEntries(entries)) {
        if ("separator" in entry) {
          const separator = document.createElement("div");
          separator.setAttribute("role", "separator");
          separator.className = "my-1 mx-1.5 h-px bg-border";
          separator.style.cssText = "height:1px;margin:0.25rem 0.375rem;background:var(--border);";
          inner.appendChild(separator);
          continue;
        }
        const item = entry;
        if (item.header === true) {
          const header = document.createElement("div");
          header.setAttribute("role", "presentation");
          header.className = "px-2 py-1.5 font-medium text-muted-foreground text-xs";
          header.textContent = item.label;
          inner.appendChild(header);
          continue;
        }

        const hasChildren = Array.isArray(item.children) && item.children.length > 0;
        const isLeafDestructive =
          !hasChildren && (item.destructive === true || item.id === ("delete" as T));
        const isDisabled = item.disabled === true;

        const button = document.createElement("button");
        button.type = "button";
        button.tabIndex = -1;
        button.setAttribute("role", "menuitem");
        // `aria-disabled`, never `disabled`: a disabled item stays in the roving
        // focus so keyboard and screen-reader users hear it and its reason
        // (WAI-ARIA APG menu pattern). The key handler refuses to choose it.
        if (isDisabled) {
          button.setAttribute("aria-disabled", "true");
        }
        if (hasChildren) {
          button.setAttribute("aria-haspopup", "menu");
          button.setAttribute("aria-expanded", "false");
        }
        if (item.description) {
          button.setAttribute("aria-description", item.description);
          button.title = item.description;
        }
        const rowBase =
          "flex w-full cursor-default select-none items-center gap-2 rounded-sm px-2 py-1 text-left outline-none transition-colors sm:min-h-7 sm:text-sm min-h-8 text-base";
        button.className = isDisabled
          ? `${rowBase} pointer-events-none cursor-not-allowed text-muted-foreground`
          : isLeafDestructive
            ? `${rowBase} text-destructive-foreground hover:bg-destructive/10 hover:text-destructive-foreground`
            : `${rowBase} text-foreground hover:bg-accent hover:text-accent-foreground`;
        button.style.cssText =
          "display:flex;width:100%;min-height:1.75rem;align-items:center;gap:0.5rem;border:0;border-radius:var(--radius-sm);background:transparent;padding:0.25rem 0.5rem;color:var(--foreground);font-family:var(--font-sans,system-ui,sans-serif);font-size:0.875rem;line-height:1.25rem;text-align:left;cursor:default;";
        if (isLeafDestructive) {
          button.style.color = "var(--destructive-foreground)";
        }
        // A disabled row dims its label, icon and chevron, never the row: opacity
        // multiplies, so dimming the row would also fade the explanation below.
        const DISABLED_OPACITY = "0.64";
        if (isDisabled) {
          button.style.color = "var(--muted-foreground)";
          button.style.pointerEvents = "none";
        }

        if (typeof item.icon === "string") {
          const icon = createIconElement(item.icon, isLeafDestructive ? "destructive" : "neutral");
          if (icon) {
            if (isDisabled) icon.style.opacity = DISABLED_OPACITY;
            button.appendChild(icon);
          }
        }

        const label = document.createElement("span");
        label.className = "min-w-0 flex-1 truncate";
        const labelText = document.createElement("span");
        labelText.textContent = item.label;
        if (isDisabled) {
          labelText.className = "opacity-64";
          labelText.style.opacity = DISABLED_OPACITY;
        }
        label.appendChild(labelText);
        if (item.description) {
          // The explanation stays at full contrast in the solid muted token (UI.md).
          const reason = document.createElement("span");
          reason.className = "block whitespace-normal text-xs text-muted-foreground";
          reason.style.cssText =
            "display:block;white-space:normal;font-size:0.75rem;line-height:1rem;color:var(--muted-foreground);";
          reason.textContent = item.description;
          label.appendChild(reason);
          // The content holds the label and the reason, so name the item by its label
          // and describe it by its reason: otherwise a screen reader reads the reason
          // twice, in the name and again as the description. `aria-description`
          // above stays as the fallback where `aria-describedby` is unsupported.
          labelText.id = menuElementId("label");
          reason.id = menuElementId("reason");
          button.setAttribute("aria-labelledby", labelText.id);
          button.setAttribute("aria-describedby", reason.id);
        }
        button.appendChild(label);

        if (hasChildren) {
          const chevron = document.createElement("span");
          chevron.className = "ms-auto shrink-0 text-muted-foreground/80 text-sm leading-none";
          chevron.textContent = ">";
          if (isDisabled) chevron.style.opacity = DISABLED_OPACITY;
          button.appendChild(chevron);
        }

        const record: RenderedMenuItem<T> = {
          element: button,
          item,
          enabled: !isDisabled,
          hasChildren,
        };
        records.push(record);

        // Pointer hover and keyboard focus share one highlight: the inline
        // styles above would otherwise override a focus utility class. A
        // disabled row takes it from keyboard focus only, on the neutral wash
        // and with its muted text, so it never looks like a choosable item.
        const restingColor = isDisabled
          ? "var(--muted-foreground)"
          : isLeafDestructive
            ? "var(--destructive-foreground)"
            : "var(--foreground)";
        const highlight = () => {
          for (const [other, clear] of unhighlightByButton) {
            if (other !== button) clear();
          }
          button.dataset.active = "true";
          button.style.background =
            isLeafDestructive && !isDisabled
              ? "color-mix(in srgb, var(--destructive) 10%, transparent)"
              : "var(--accent)";
          button.style.color =
            isDisabled || isLeafDestructive ? restingColor : "var(--accent-foreground)";
        };
        const unhighlight = () => {
          button.dataset.active = "false";
          button.style.background = "transparent";
          button.style.color = restingColor;
        };
        unhighlightByButton.set(button, unhighlight);
        button.addEventListener("focus", highlight);
        button.addEventListener("blur", unhighlight);

        if (!isDisabled) {
          // One active item for pointer and keyboard: hovering moves focus, so
          // Enter always runs the highlighted item and only one row looks active.
          button.addEventListener("mouseenter", () => {
            if (typeof button.focus === "function") {
              button.focus({ preventScroll: true });
            }
            highlight();
          });
          button.addEventListener("mouseleave", () => {
            if (typeof document !== "undefined" && document.activeElement === button) {
              return;
            }
            unhighlight();
          });

          if (hasChildren) {
            button.addEventListener("mouseenter", () => {
              openSubmenu(record, level, false);
            });
            button.addEventListener("click", (event) => {
              event.preventDefault();
            });
          } else {
            button.addEventListener("mouseenter", () => {
              closeMenusFromLevel(level + 1);
            });
            button.addEventListener("click", () => cleanup(item.id, { restoreFocus: true }));
          }
        }

        inner.appendChild(button);
      }

      menu.appendChild(inner);

      menu.addEventListener("mouseenter", () => {
        closeMenusFromLevel(level + 1);
      });

      // Insert hidden, measure and clamp synchronously, then reveal: the first
      // painted frame is already inside the viewport.
      menu.style.visibility = "hidden";
      document.body.appendChild(menu);
      menuStack[level] = menu;
      renderedItemsByLevel[level] = records;
      let left = preferredLeft;
      if (leftWhenOverflowing) {
        const unclamped = menu.getBoundingClientRect();
        if (unclamped.right > window.innerWidth) {
          left = leftWhenOverflowing(unclamped.width);
        }
      }
      clampMenuPosition(menu, left, preferredTop);
      menu.style.visibility = "visible";
    };

    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("contextmenu", onContextMenu, true);
    openMenu(items, position?.x ?? 0, position?.y ?? 0, 0);
    focusOpeningItem(0);

    requestAnimationFrame(() => {
      canDismissFromPointer = true;
    });
  });
}
