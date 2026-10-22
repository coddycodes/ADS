// @vitest-environment happy-dom

import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { Dialog, DialogClose, DialogPopup, DialogTitle } from "./dialog";

let container: HTMLDivElement;
let root: Root;
let originalGetAnimations: PropertyDescriptor | undefined;

function FormDialog(props: {
  readonly onOpenChange: (open: boolean) => void;
  readonly disablePointerDismissal?: boolean;
}) {
  const [open, setOpen] = useState(true);
  if (!open) return null;
  return (
    <Dialog
      open
      onOpenChange={(next) => {
        props.onOpenChange(next);
        setOpen(next);
      }}
      {...(props.disablePointerDismissal === undefined
        ? {}
        : { disablePointerDismissal: props.disablePointerDismissal })}
    >
      <DialogPopup>
        <DialogTitle>Describe the change</DialogTitle>
        <input aria-label="Description" defaultValue="" />
        <DialogClose>Cancel</DialogClose>
      </DialogPopup>
    </Dialog>
  );
}

async function mount(props: Parameters<typeof FormDialog>[0]) {
  await act(async () => root.render(<FormDialog {...props} />));
}

function description(): HTMLInputElement | null {
  return document.querySelector<HTMLInputElement>('input[aria-label="Description"]');
}

async function typeDescription(value: string) {
  await act(async () => {
    const field = description();
    if (!field) throw new Error("Missing description field");
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(field, value);
    field.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function pressOutside() {
  const backdrop = document.querySelector('[data-slot="dialog-backdrop"]');
  if (!backdrop) throw new Error("Missing dialog backdrop");
  await act(async () => {
    for (const type of ["pointerdown", "mousedown", "pointerup", "mouseup", "click"]) {
      const init = { bubbles: true, cancelable: true, button: 0, pointerType: "mouse" };
      backdrop.dispatchEvent(
        type.startsWith("pointer") ? new PointerEvent(type, init) : new MouseEvent(type, init),
      );
    }
  });
}

async function pressEscape() {
  await act(async () => {
    (document.activeElement ?? document.body).dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
    );
  });
}

function button(label: string): HTMLButtonElement {
  const match = [...document.querySelectorAll<HTMLButtonElement>("button")].find(
    (candidate) =>
      candidate.getAttribute("aria-label") === label || candidate.textContent?.trim() === label,
  );
  if (!match) throw new Error(`Missing button: ${label}`);
  return match;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  originalGetAnimations = Object.getOwnPropertyDescriptor(Element.prototype, "getAnimations");
  Object.defineProperty(Element.prototype, "getAnimations", {
    configurable: true,
    value: () => [],
  });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  document.body.replaceChildren();
  if (originalGetAnimations) {
    Object.defineProperty(Element.prototype, "getAnimations", originalGetAnimations);
  } else {
    Reflect.deleteProperty(Element.prototype, "getAnimations");
  }
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("Dialog dismissal", () => {
  it("stays open with its input intact after an outside press", async () => {
    const onOpenChange = vi.fn();
    await mount({ onOpenChange });
    await typeDescription("Half-written description");

    await pressOutside();

    expect(onOpenChange).not.toHaveBeenCalled();
    expect(description()?.value).toBe("Half-written description");
  });

  it("still closes on Escape", async () => {
    const onOpenChange = vi.fn();
    await mount({ onOpenChange });
    await act(async () => description()?.focus());

    await pressEscape();

    expect(onOpenChange).toHaveBeenCalledWith(false);
    expect(description()).toBeNull();
  });

  it.each(["Close", "Cancel"])("still closes from its %s button", async (label) => {
    const onOpenChange = vi.fn();
    await mount({ onOpenChange });

    await act(async () => button(label).click());

    expect(onOpenChange).toHaveBeenCalledWith(false);
    expect(description()).toBeNull();
  });

  it("closes on an outside press when a dialog opts in to light dismissal", async () => {
    const onOpenChange = vi.fn();
    await mount({ onOpenChange, disablePointerDismissal: false });

    await pressOutside();

    expect(onOpenChange).toHaveBeenCalledWith(false);
    expect(description()).toBeNull();
  });
});
