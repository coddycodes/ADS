// @vitest-environment happy-dom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { FileEditorToolbar } from "./FileEditorToolbar";
import { TooltipProvider } from "../ui/tooltip";

describe("FileEditorToolbar", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.useRealTimers();
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
  });

  it("renders Save, Undo, Redo in order and invokes enabled actions", () => {
    const onSave = vi.fn();
    const onUndo = vi.fn();
    const onRedo = vi.fn();
    act(() => {
      root.render(
        <FileEditorToolbar
          savePhase="pending"
          confirmedRevision={0}
          canSave
          canUndo
          canRedo={false}
          cleanStatus={null}
          onSave={onSave}
          onUndo={onUndo}
          onRedo={onRedo}
        />,
      );
    });

    const buttons = [...container.querySelectorAll("button")];
    expect(buttons.map((button) => button.getAttribute("aria-label"))).toEqual([
      "Save file",
      "Undo",
      "Redo",
    ]);
    expect(buttons[0]!.disabled).toBe(false);
    expect(buttons[1]!.disabled).toBe(false);
    expect(buttons[2]!.disabled).toBe(true);
    buttons[0]!.click();
    buttons[1]!.click();
    expect(onSave).toHaveBeenCalledOnce();
    expect(onUndo).toHaveBeenCalledOnce();
    expect(onRedo).not.toHaveBeenCalled();
    expect(container.textContent).toContain("Unsaved changes");
  });

  it("renders the Markdown view toggle after editing actions", () => {
    const onRenderedChange = vi.fn();
    const render = (rendered: boolean) => {
      act(() => {
        root.render(
          <TooltipProvider delay={0}>
            <FileEditorToolbar
              savePhase="clean"
              confirmedRevision={0}
              canSave={false}
              canUndo={false}
              canRedo={false}
              cleanStatus={null}
              markdownView={{ rendered, onRenderedChange }}
              onSave={vi.fn()}
              onUndo={vi.fn()}
              onRedo={vi.fn()}
            />
          </TooltipProvider>,
        );
      });
    };

    render(true);

    let buttons = [...container.querySelectorAll("button")];
    expect(buttons.map((button) => button.getAttribute("aria-label"))).toEqual([
      "Save file",
      "Undo",
      "Redo",
      "Show markdown source",
    ]);
    expect(container.querySelector('[role="separator"]')).not.toBeNull();
    expect(buttons[3]!.getAttribute("aria-pressed")).toBe("true");
    expect(buttons[3]!.querySelector(".lucide-code-xml")).not.toBeNull();

    buttons[3]!.click();
    expect(onRenderedChange).toHaveBeenCalledWith(false);

    onRenderedChange.mockClear();
    render(false);
    buttons = [...container.querySelectorAll("button")];
    expect(buttons[3]!.getAttribute("aria-label")).toBe("Show rendered markdown");
    expect(buttons[3]!.getAttribute("aria-pressed")).toBe("false");
    expect(buttons[3]!.querySelector(".lucide-eye")).not.toBeNull();
    buttons[3]!.click();
    expect(onRenderedChange).toHaveBeenCalledWith(true);
  });

  it("shows saving, retry, read-only, and transient saved statuses", () => {
    vi.useFakeTimers();
    const render = (
      savePhase: "clean" | "pending" | "saving" | "failed",
      confirmedRevision: number,
      cleanStatus: string | null,
    ) => {
      act(() => {
        root.render(
          <FileEditorToolbar
            savePhase={savePhase}
            confirmedRevision={confirmedRevision}
            canSave={savePhase === "pending" || savePhase === "failed"}
            canUndo={false}
            canRedo={false}
            cleanStatus={cleanStatus}
            onSave={vi.fn()}
            onUndo={vi.fn()}
            onRedo={vi.fn()}
          />,
        );
      });
    };

    render("saving", 0, null);
    expect(container.textContent).toContain("Saving…");
    render("failed", 0, null);
    expect(container.textContent).toContain("Save failed — retry");
    render("clean", 0, "Editing unavailable");
    expect(container.textContent).toContain("Editing unavailable");
    render("clean", 1, null);
    expect(container.textContent).toContain("Saved");
    act(() => vi.advanceTimersByTime(1_500));
    expect(container.textContent).not.toContain("Saved");
  });

  it("still clears Saved when another file with a lower revision opens during it", () => {
    vi.useFakeTimers();
    const render = (confirmedRevision: number) => {
      act(() => {
        root.render(
          <FileEditorToolbar
            savePhase="clean"
            confirmedRevision={confirmedRevision}
            canSave={false}
            canUndo={false}
            canRedo={false}
            cleanStatus={null}
            onSave={vi.fn()}
            onUndo={vi.fn()}
            onRedo={vi.fn()}
          />,
        );
      });
    };

    render(4);
    render(5);
    expect(container.textContent).toContain("Saved");
    render(2);
    act(() => vi.advanceTimersByTime(1_500));
    expect(container.textContent).not.toContain("Saved");
  });

  it("keeps native disabled buttons inside focusable hover tooltip triggers", async () => {
    act(() => {
      root.render(
        <TooltipProvider delay={0}>
          <FileEditorToolbar
            savePhase="clean"
            confirmedRevision={0}
            canSave={false}
            canUndo={false}
            canRedo={false}
            cleanStatus={null}
            onSave={vi.fn()}
            onUndo={vi.fn()}
            onRedo={vi.fn()}
          />
        </TooltipProvider>,
      );
    });

    const buttons = [...container.querySelectorAll("button")];
    const saveTrigger = buttons[0]!.parentElement!;
    const redoTrigger = buttons[2]!.parentElement!;
    expect(buttons.every((button) => button.disabled)).toBe(true);
    expect(saveTrigger.getAttribute("data-slot")).toBe("tooltip-trigger");
    expect(saveTrigger.tabIndex).toBe(0);

    await act(async () => {
      redoTrigger.focus();
      await Promise.resolve();
    });
    expect(document.body.querySelector('[data-slot="tooltip-popup"]')?.textContent).toContain(
      "Redo (Shift+Ctrl/Cmd+Z)",
    );

    await act(async () => {
      redoTrigger.blur();
      saveTrigger.dispatchEvent(new PointerEvent("pointerenter", { pointerType: "mouse" }));
      saveTrigger.dispatchEvent(new MouseEvent("mouseenter"));
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(
      [...document.body.querySelectorAll('[data-slot="tooltip-popup"]')].some((popup) =>
        popup.textContent?.includes("Save file (Ctrl/Cmd+S)"),
      ),
    ).toBe(true);
  });

  it("uses foreground classes for enabled actions and muted foreground for disabled actions", () => {
    act(() => {
      root.render(
        <FileEditorToolbar
          savePhase="pending"
          confirmedRevision={0}
          canSave
          canUndo
          canRedo={false}
          cleanStatus={null}
          onSave={vi.fn()}
          onUndo={vi.fn()}
          onRedo={vi.fn()}
        />,
      );
    });

    const [save, undo, redo] = [...container.querySelectorAll("button")];
    expect(save!.className.split(/\s+/)).toContain("text-foreground");
    expect(save!.className.split(/\s+/)).not.toContain("text-muted-foreground");
    expect(undo!.className.split(/\s+/)).toContain("text-foreground");
    expect(undo!.className.split(/\s+/)).not.toContain("text-muted-foreground");
    expect(redo!.className.split(/\s+/)).toContain("text-muted-foreground");
  });
});
