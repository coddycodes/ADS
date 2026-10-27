// @vitest-environment happy-dom
import { ProviderDriverKind } from "@coddy/contracts";
import { act, cloneElement, type ReactElement, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vite-plus/test";

vi.mock("../ui/tooltip", () => ({
  Tooltip: ({ children }: { children?: ReactNode }) => <>{children}</>,
  TooltipPopup: ({ children }: { children?: ReactNode }) => <span data-tooltip="">{children}</span>,
  TooltipTrigger: ({ render, children }: { render?: ReactElement; children?: ReactNode }) =>
    render ? cloneElement(render, undefined, children) : <>{children}</>,
}));
vi.mock("../ui/sidebar", () => ({
  SidebarMenuSubItem: ({ children, ...props }: React.ComponentProps<"li">) => (
    <li {...props}>{children}</li>
  ),
}));
vi.mock("../ThreadStatusIndicators", () => ({
  ChangeRequestStatusIcon: ({ className }: { className?: string }) => (
    <svg data-icon="change-request" className={className} />
  ),
}));

import { WORKSPACE_CARD_STATUS } from "../Sidebar.logic";
import {
  isWorkspaceCardControlTarget,
  WorkspaceCardBranchLine,
  WorkspaceCardPrButton,
  WorkspaceCardPortsButton,
  WorkspaceCardMoreChats,
  WorkspaceCardSessionLine,
  WorkspaceCardShell,
  WorkspaceCardStatusGlyph,
  WorkspaceCardTitleLine,
} from "./WorkspaceCard";

const noop = () => {};

describe("WorkspaceCardStatusGlyph", () => {
  it.each([
    ["approval", "lucide-hand"],
    ["input", "lucide-circle-question-mark"],
    ["working", "lucide-loader-circle"],
    ["connecting", "lucide-loader-circle"],
    ["failed", "lucide-triangle-alert"],
    ["plan", "lucide-list-checks"],
  ] as const)("draws %s with its icon and accessible name", (key, iconClass) => {
    const status = WORKSPACE_CARD_STATUS[key];
    const markup = renderToStaticMarkup(<WorkspaceCardStatusGlyph status={status} />);
    expect(markup).toContain('role="img"');
    expect(markup).toContain(`aria-label="${status.label}"`);
    expect(markup).toContain(`data-status="${status.kind}"`);
    expect(markup).toContain(iconClass);
    expect(markup).toContain(status.colorClass.split(" ")[0]!);
  });

  it("spins only when motion is allowed", () => {
    expect(
      renderToStaticMarkup(<WorkspaceCardStatusGlyph status={WORKSPACE_CARD_STATUS.working} />),
    ).toContain("motion-safe:animate-spin");
  });

  it("draws the finished dot filled and the idle ring hollow", () => {
    expect(
      renderToStaticMarkup(<WorkspaceCardStatusGlyph status={WORKSPACE_CARD_STATUS.done} />),
    ).toContain('r="4" fill="currentColor"');
    expect(
      renderToStaticMarkup(<WorkspaceCardStatusGlyph status={WORKSPACE_CARD_STATUS.idle} />),
    ).toContain('r="3.5" fill="none"');
  });
});

describe("WorkspaceCardShell", () => {
  it("keeps PR and ports as focusable sibling controls without activating the card", async () => {
    const openCard = vi.fn();
    const openPr = vi.fn();
    const openPorts = vi.fn();
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    try {
      await act(async () =>
        root.render(
          <WorkspaceCardShell
            testId="card"
            buttonTestId="card-button"
            className=""
            idBase="controls"
            isActive={false}
            hasFlags={false}
            hasBranchLine
            hasSessionLine={false}
            status={WORKSPACE_CARD_STATUS.idle}
            onClick={(event) => {
              if (!isWorkspaceCardControlTarget(event.target)) openCard();
            }}
            onContextMenu={noop}
            onButtonKeyDown={noop}
          >
            <WorkspaceCardPrButton
              indicator={{
                url: "https://example.invalid/1",
                tooltip: "Open MR !1",
                label: "MR open",
                colorClass: "text-foreground",
                numberLabel: "!1",
              }}
              onClick={openPr}
            />
            <WorkspaceCardPortsButton ports={[{ port: 3000 }]} onClick={openPorts} />
          </WorkspaceCardShell>,
        ),
      );
      const card = container.querySelector<HTMLButtonElement>('[data-testid="card-button"]')!;
      const pr = container.querySelector<HTMLButtonElement>('[aria-label="Open MR !1"]')!;
      const ports = container.querySelector<HTMLButtonElement>(
        '[aria-label="Open localhost:3000"]',
      )!;
      for (const control of [pr, ports]) {
        expect(card.contains(control)).toBe(false);
        control.focus();
        expect(document.activeElement).toBe(control);
        await act(async () => control.click());
      }
      expect(openPr).toHaveBeenCalledOnce();
      expect(openPorts).toHaveBeenCalledOnce();
      expect(openCard).not.toHaveBeenCalled();
      card.focus();
      expect(document.activeElement).toBe(card);
      await act(async () => card.click());
      expect(openCard).toHaveBeenCalledOnce();
    } finally {
      await act(async () => root.unmount());
      container.remove();
    }
  });

  it("names the one button by status, title and flags, and describes it by lines 2–3", () => {
    const markup = renderToStaticMarkup(
      <WorkspaceCardShell
        testId="thread-row-a"
        buttonTestId="thread-card-button-a"
        className="card-surface"
        idBase="card"
        isActive
        hasFlags
        hasBranchLine
        hasSessionLine={false}
        status={WORKSPACE_CARD_STATUS.working}
        onClick={noop}
        onContextMenu={noop}
        onButtonKeyDown={noop}
      >
        <span>lines</span>
      </WorkspaceCardShell>,
    );
    expect(markup).toContain('data-testid="thread-row-a"');
    expect(markup).toContain('data-thread-item="true"');
    expect(markup).toContain('data-testid="thread-card-button-a"');
    expect(markup).toContain('type="button"');
    expect(markup).toContain('aria-labelledby="card-status card-title card-flags"');
    expect(markup).toContain('aria-describedby="card-branch"');
    expect(markup).toContain('aria-current="page"');
    expect(markup).toContain('id="card-status"');
    expect(markup).toContain("pointer-events-none relative z-10 flex gap-2 px-2 py-1.5");
    expect(markup).toContain("flex h-5 w-4 shrink-0 items-center justify-center");
  });

  it("omits the description and aria-current when there is nothing to describe", () => {
    const markup = renderToStaticMarkup(
      <WorkspaceCardShell
        testId="thread-row-b"
        buttonTestId="thread-card-button-b"
        className=""
        idBase="card"
        isActive={false}
        hasFlags={false}
        hasBranchLine={false}
        hasSessionLine={false}
        status={WORKSPACE_CARD_STATUS.idle}
        onClick={noop}
        onContextMenu={noop}
        onButtonKeyDown={noop}
      >
        {null}
      </WorkspaceCardShell>,
    );
    expect(markup).toContain('aria-labelledby="card-status card-title"');
    expect(markup).not.toContain("aria-describedby");
    expect(markup).not.toContain("aria-current");
  });
});

describe("card lines", () => {
  it("keeps 13 px icons inside PR and ports targets of at least 24 px", () => {
    const markup = renderToStaticMarkup(
      <>
        <WorkspaceCardPrButton
          indicator={{
            url: "https://example.invalid/57",
            tooltip: "Open MR !57",
            label: "MR open",
            colorClass: "text-foreground",
            numberLabel: "!57",
          }}
          onClick={noop}
        />
        <WorkspaceCardPortsButton ports={[{ port: 3000 }]} onClick={noop} />
      </>,
    );
    expect(markup.match(/min-h-6 min-w-6/g)).toHaveLength(2);
    expect(markup.match(/size-\[13px\]/g)).toHaveLength(2);
  });

  it("renders unknown-provider initials at text-xs through the real imported icon", () => {
    const markup = renderToStaticMarkup(
      <WorkspaceCardSessionLine
        id="s"
        provider={{ driverKind: ProviderDriverKind.make("custom"), label: "Custom Provider" }}
        preview={{ text: "Ready", tone: null }}
        model="custom"
        ageIso={null}
      />,
    );
    expect(markup).toContain(">CP</span>");
    expect(markup).toContain("text-xs");
    expect(markup).not.toMatch(/text-\[(?:8|9|10|11)px\]/);
  });

  it("bolds an unread title and announces unread and pinned", () => {
    const markup = renderToStaticMarkup(
      <WorkspaceCardTitleLine
        id="t"
        flagsId="f"
        title="Upgrade PDF renderer"
        titleTestId="thread-title-a"
        unread
        pinned
        pinnedTestId="thread-pinned-a"
      />,
    );
    expect(markup).toContain("font-semibold text-foreground");
    expect(markup).toContain('data-unread="true"');
    expect(markup).toContain('id="f" class="sr-only">unread, pinned</span>');
    expect(markup).toContain('data-testid="thread-pinned-a"');
  });

  it("shows a read title muted with the primary chip", () => {
    const markup = renderToStaticMarkup(
      <WorkspaceCardTitleLine
        id="t"
        flagsId="f"
        title="develop"
        titleTestId="primary-card-title-p"
        unread={false}
        pinned={false}
        primary
      />,
    );
    expect(markup).toContain("text-[13px] text-foreground/80");
    expect(markup).toContain(">primary<");
    expect(markup).not.toContain("sr-only");
  });

  it("keeps the indicators right-aligned when the branch text is hidden", () => {
    const hidden = renderToStaticMarkup(
      <WorkspaceCardBranchLine id="b" branch={null} branchTooltip={null}>
        <span>indicator</span>
      </WorkspaceCardBranchLine>,
    );
    expect(hidden).not.toContain("lucide-git-branch");
    expect(hidden).toContain('<span class="flex-1"></span><span>indicator</span>');
    const shown = renderToStaticMarkup(
      <WorkspaceCardBranchLine
        id="b"
        branch="fix-TRI-150"
        branchTooltip="Worktree: fix-TRI-150 (fix-TRI-150)"
      />,
    );
    expect(shown).toContain("lucide-git-branch");
    expect(shown).toContain("fix-TRI-150");
    expect(shown).toContain("h-[18px]");
  });

  it("replaces the branch with a compact notice and describes the full reason", () => {
    const description = "Git cannot read this repository. Check its .git folder.";
    const container = document.createElement("div");
    container.innerHTML = renderToStaticMarkup(
      <WorkspaceCardBranchLine
        id="unavailable-branch"
        branch="main"
        branchTooltip="Branch: main"
        notice={{ label: "Repository unreadable", description }}
      >
        <span data-indicator="">terminal</span>
      </WorkspaceCardBranchLine>,
    );

    const line = container.querySelector("#unavailable-branch")!;
    const label = line.querySelector('span[aria-hidden="true"]')!;
    expect(label?.textContent).toBe("Repository unreadable");
    expect(label.classList.contains("truncate")).toBe(true);
    expect(line.querySelector(".sr-only")?.textContent).toBe(description);
    expect(line.querySelector("[data-tooltip]")?.textContent).toBe(description);
    expect(line.querySelector(".lucide-git-branch")).toBeNull();
    const icon = line.querySelector(".lucide-circle-alert")!;
    expect(icon?.getAttribute("aria-hidden")).toBe("true");
    expect(icon.classList.contains("size-3")).toBe(true);
    expect(icon.classList.contains("shrink-0")).toBe(true);
    expect(line.textContent).not.toContain("main");
    expect(line.lastElementChild?.getAttribute("data-indicator")).toBe("");
  });

  it("renders line 3 with the preview tone, a mono model and no age without a source", () => {
    const markup = renderToStaticMarkup(
      <WorkspaceCardSessionLine
        id="s"
        provider={{ driverKind: null, label: null }}
        preview={{ text: "Delivery failed", tone: "destructive" }}
        model="opus"
        ageIso={null}
      />,
    );
    expect(markup).toContain("text-destructive");
    expect(markup).toContain("Delivery failed");
    expect(markup).toContain("max-w-28 shrink-0 truncate font-mono text-xs");
    expect(markup).toContain(">opus<");
    expect(markup).not.toContain("<time");
  });

  it("says how many more chats without a chevron", () => {
    const two = renderToStaticMarkup(
      <WorkspaceCardMoreChats count={2} status={WORKSPACE_CARD_STATUS.idle} />,
    );
    expect(two).toContain("2 more chats");
    expect(two).not.toContain("lucide-chevron-right");
    expect(
      renderToStaticMarkup(
        <WorkspaceCardMoreChats count={1} status={WORKSPACE_CARD_STATUS.done} />,
      ),
    ).toContain("1 more chat<");
  });
});

describe("isWorkspaceCardControlTarget", () => {
  it("recognises events from the card's own controls", () => {
    expect(isWorkspaceCardControlTarget({ closest: () => ({}) } as unknown as EventTarget)).toBe(
      true,
    );
    expect(isWorkspaceCardControlTarget({ closest: () => null } as unknown as EventTarget)).toBe(
      false,
    );
    expect(isWorkspaceCardControlTarget(null)).toBe(false);
  });
});
