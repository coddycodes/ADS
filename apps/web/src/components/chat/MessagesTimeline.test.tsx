import {
  CheckpointRef,
  EnvironmentId,
  MessageId,
  ProviderDriverKind,
  ProviderInstanceId,
  TurnId,
} from "@bibcode/contracts";
import { act, createRef, type ComponentProps, type ReactNode, type Ref } from "react";
import type { WorkLogEntry } from "../../session-logic";
import { renderToStaticMarkup } from "react-dom/server";
import { createRoot, type Root } from "react-dom/client";
import { Window } from "happy-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { LegendListRef } from "@legendapp/list/react";

let MessagesTimelineComponent: typeof import("./MessagesTimeline").MessagesTimeline | null = null;
const SLOW_MESSAGES_TIMELINE_IMPORT_TIMEOUT_MS = 60_000;

const h = vi.hoisted(() => ({
  legendListProps: null as Record<string, unknown> | null,
}));
let domWindow: Window | null = null;

interface MountedTree {
  readonly container: HTMLDivElement;
  readonly root: Root;
}

const mountedTrees: MountedTree[] = [];

vi.mock("@legendapp/list/react", async () => {
  const { useEffect } = await import("react");
  const legendListTestId = "legend-list";

  const LegendList = (props: {
    data: Array<{ id: string }>;
    keyExtractor: (item: { id: string }) => string;
    getItemType?: (item: { id: string }) => string;
    renderItem: (args: { item: { id: string } }) => ReactNode;
    onScroll?: () => void;
    ListHeaderComponent?: ReactNode;
    ListFooterComponent?: ReactNode;
    anchoredEndSpace?: {
      anchorIndex: number;
      anchorMaxSize?: number;
      anchorOffset?: number;
      onReady?: (info: { anchorIndex: number }) => void;
      onSizeChanged?: (size: number) => void;
    };
    contentInsetEndAdjustment?: number;
    className?: string;
    maintainScrollAtEnd?:
      | boolean
      | {
          animated?: boolean;
          on?: {
            dataChange?: boolean;
            itemLayout?: boolean;
            layout?: boolean;
          };
        };
    maintainVisibleContentPosition?:
      | boolean
      | {
          data?: boolean;
          size?: boolean;
          shouldRestorePosition?: (item: { id: string }) => boolean;
        };
    ref?: Ref<LegendListRef>;
  }) => {
    useEffect(() => {
      h.legendListProps = props as unknown as Record<string, unknown>;
    });
    if (props.anchoredEndSpace) {
      props.anchoredEndSpace.onSizeChanged?.(240);
      props.anchoredEndSpace.onReady?.({ anchorIndex: props.anchoredEndSpace.anchorIndex });
    }
    // Simulate the initial scroll callback LegendList fires after layout.
    props.onScroll?.();
    return (
      <div
        data-testid={legendListTestId}
        data-anchor-index={props.anchoredEndSpace?.anchorIndex}
        data-anchor-max-size={props.anchoredEndSpace?.anchorMaxSize}
        data-anchor-offset={props.anchoredEndSpace?.anchorOffset}
        data-anchor-on-ready={Boolean(props.anchoredEndSpace?.onReady)}
        data-content-inset-end={props.contentInsetEndAdjustment}
        data-class-name={props.className}
        data-maintain-scroll-at-end={props.maintainScrollAtEnd ? "enabled" : undefined}
        data-maintain-scroll-at-end-animated={
          typeof props.maintainScrollAtEnd === "object"
            ? props.maintainScrollAtEnd.animated
            : undefined
        }
        data-maintain-scroll-at-end-data-change={
          typeof props.maintainScrollAtEnd === "object"
            ? props.maintainScrollAtEnd.on?.dataChange
            : undefined
        }
        data-maintain-scroll-at-end-item-layout={
          typeof props.maintainScrollAtEnd === "object"
            ? props.maintainScrollAtEnd.on?.itemLayout
            : undefined
        }
        data-maintain-scroll-at-end-layout={
          typeof props.maintainScrollAtEnd === "object"
            ? props.maintainScrollAtEnd.on?.layout
            : undefined
        }
        data-maintain-visible-content-position={
          typeof props.maintainVisibleContentPosition === "object"
            ? "object"
            : props.maintainVisibleContentPosition
        }
        data-maintain-visible-content-position-data={
          typeof props.maintainVisibleContentPosition === "object"
            ? props.maintainVisibleContentPosition.data
            : undefined
        }
        data-maintain-visible-content-position-size={
          typeof props.maintainVisibleContentPosition === "object"
            ? props.maintainVisibleContentPosition.size
            : undefined
        }
      >
        {props.ListHeaderComponent}
        {props.data.map((item) => (
          <div key={props.keyExtractor(item)} data-item-type={props.getItemType?.(item)}>
            {props.renderItem({ item })}
          </div>
        ))}
        {props.ListFooterComponent}
      </div>
    );
  };

  return { LegendList };
});

function MockFileDiff(props: {
  fileDiff: { name?: string | null; prevName?: string | null };
  renderCustomHeader?: (fileDiff: {
    name?: string | null;
    prevName?: string | null;
  }) => React.ReactNode;
}) {
  return (
    <div data-testid="file-diff">
      {props.renderCustomHeader?.(props.fileDiff)}
      {props.fileDiff.name ?? props.fileDiff.prevName ?? "diff"}
    </div>
  );
}

vi.mock("@pierre/diffs/react", () => {
  return { FileDiff: MockFileDiff };
});

beforeEach(async () => {
  domWindow = new Window({ url: "https://bibcode.test/" });
  vi.stubGlobal("window", domWindow);
  vi.stubGlobal("document", domWindow.document);
  vi.stubGlobal("navigator", domWindow.navigator);
  vi.stubGlobal("localStorage", domWindow.localStorage);
  vi.stubGlobal("Node", domWindow.Node);
  vi.stubGlobal("Element", domWindow.Element);
  vi.stubGlobal("HTMLElement", domWindow.HTMLElement);
  vi.stubGlobal("Event", domWindow.Event);
  vi.stubGlobal("MouseEvent", domWindow.MouseEvent);
  vi.stubGlobal("KeyboardEvent", domWindow.KeyboardEvent);
  vi.stubGlobal("CustomEvent", domWindow.CustomEvent);
  vi.stubGlobal("customElements", domWindow.customElements);
  vi.stubGlobal("MutationObserver", domWindow.MutationObserver);
  vi.stubGlobal("ResizeObserver", domWindow.ResizeObserver);
  vi.stubGlobal("getComputedStyle", domWindow.getComputedStyle.bind(domWindow));
  vi.stubGlobal("requestAnimationFrame", domWindow.requestAnimationFrame.bind(domWindow));
  vi.stubGlobal("cancelAnimationFrame", domWindow.cancelAnimationFrame.bind(domWindow));
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  MessagesTimelineComponent ??= (await import("./MessagesTimeline")).MessagesTimeline;
}, SLOW_MESSAGES_TIMELINE_IMPORT_TIMEOUT_MS);

afterEach(async () => {
  for (const mounted of mountedTrees.splice(0)) {
    await act(async () => mounted.root.unmount());
    mounted.container.remove();
  }
  document.body.replaceChildren();
  vi.restoreAllMocks();
  domWindow?.close();
  domWindow = null;
  vi.unstubAllGlobals();
});

const ACTIVE_THREAD_ENVIRONMENT_ID = EnvironmentId.make("environment-local");
const MESSAGE_CREATED_AT = "2026-03-17T19:12:28.000Z";

async function loadMessagesTimeline() {
  return MessagesTimelineComponent ?? (await import("./MessagesTimeline")).MessagesTimeline;
}

function buildProps() {
  return {
    isWorking: false,
    activeTurnInProgress: false,
    activeTurnStartedAt: null,
    listRef: createRef<LegendListRef | null>(),
    latestTurn: null,
    runningTurnId: null,
    turnDiffSummaryByAssistantMessageId: new Map(),
    routeThreadKey: "environment-local:thread-1",
    onOpenTurnDiff: () => {},
    revertTurnCountByUserMessageId: new Map(),
    onRevertUserMessage: () => {},
    onResolveTurnDelivery: () => {},
    resolvingTurnDeliveryMessageId: null,
    blockingDelivery: null,
    instanceLabels: new Map<ProviderInstanceId, string>(),
    queuedMessages: [],
    queuedStatuses: [],
    onSteerQueuedMessage: () => {},
    onSendNowQueuedMessage: () => {},
    onCancelQueuedMessage: () => {},
    resolvingQueuedMessageId: null,
    queuedMessageErrors: {},
    attachmentUploads: {},
    onCancelAttachmentUpload: () => {},
    isRevertingCheckpoint: false,
    onImageExpand: () => {},
    activeThreadEnvironmentId: ACTIVE_THREAD_ENVIRONMENT_ID,
    markdownCwd: undefined,
    resolvedTheme: "light" as const,
    timestampFormat: "locale" as const,
    workspaceRoot: undefined,
    anchorMessageId: null,
    onAnchorReady: () => {},
    onAnchorSizeChanged: () => {},
    contentInsetEndAdjustment: 0,
    onIsAtEndChange: () => {},
    onManualNavigation: () => {},
  };
}

function buildLongUserMessageText(tail = "deep hidden detail only after expand") {
  return Array.from({ length: 9 }, (_, index) =>
    index === 8 ? tail : `Line ${index + 1}: ${"verbose prompt content ".repeat(8).trim()}`,
  ).join("\n");
}

function buildUserTimelineEntry(text: string) {
  return {
    id: "entry-1",
    kind: "message" as const,
    createdAt: MESSAGE_CREATED_AT,
    message: {
      id: MessageId.make("message-1"),
      role: "user" as const,
      text,
      turnId: null,
      createdAt: MESSAGE_CREATED_AT,
      updatedAt: MESSAGE_CREATED_AT,
      streaming: false,
    },
  };
}

describe("MessagesTimeline", () => {
  it.each([
    [true, "Waiting for an earlier message. Retry or dismiss it to send this one."],
    [false, "Waiting for an earlier message. Dismiss it to send this one."],
  ] as const)(
    "explains a pending message behind a failure (offersRetry=%s)",
    async (offersRetry, text) => {
      const failed = buildUserTimelineEntry("Earlier message");
      const pending = buildUserTimelineEntry("Later message");
      const markup = await renderTimeline({
        blockingDelivery: { ...failed.message, offersRetry },
        timelineEntries: [
          {
            ...failed,
            message: {
              ...failed.message,
              delivery: {
                state: "failed",
                provider: ProviderDriverKind.make("codex"),
                ...(offersRetry ? {} : { reason: "modelSelectionRefused" as const }),
              },
            },
          },
          {
            ...pending,
            id: "pending-entry",
            createdAt: "2026-03-17T19:12:29.000Z",
            message: {
              ...pending.message,
              id: MessageId.make("pending-message"),
              createdAt: "2026-03-17T19:12:29.000Z",
              delivery: { state: "pending", provider: ProviderDriverKind.make("codex") },
            },
          },
        ],
      });

      expect(markup).toContain(text);
      expect(markup.indexOf("Later message")).toBeLessThan(markup.indexOf(text));
    },
  );

  it("does not mark a pending message older than the blocker as waiting", async () => {
    const pending = buildUserTimelineEntry("Earlier pending message");
    const markup = await renderTimeline({
      blockingDelivery: {
        id: MessageId.make("blocker"),
        createdAt: "2026-03-17T19:12:29.000Z",
        offersRetry: true,
      },
      timelineEntries: [
        {
          ...pending,
          message: {
            ...pending.message,
            delivery: { state: "pending", provider: ProviderDriverKind.make("codex") },
          },
        },
      ],
    });

    expect(markup).not.toContain("Waiting for an earlier message");
  });

  it("names the routed provider instance in a failed notice", async () => {
    const entry = buildUserTimelineEntry("Send to my personal instance.");
    const instanceId = ProviderInstanceId.make("codex-personal");
    const markup = await renderTimeline({
      instanceLabels: new Map([[instanceId, "Codex Personal"]]),
      timelineEntries: [
        {
          ...entry,
          message: {
            ...entry.message,
            delivery: {
              state: "failed",
              provider: ProviderDriverKind.make("codex"),
              providerInstanceId: instanceId,
            },
          },
        },
      ],
    });

    expect(markup).toContain("Codex Personal did not receive this message");
  });

  it("renders delivery uncertainty immediately below the affected user message", async () => {
    const MessagesTimeline = await loadMessagesTimeline();
    const entry = buildUserTimelineEntry("Send this once.");
    const markup = renderToStaticMarkup(
      <MessagesTimeline
        {...buildProps()}
        timelineEntries={[
          {
            ...entry,
            message: {
              ...entry.message,
              delivery: {
                state: "uncertain" as const,
                provider: ProviderDriverKind.make("claudeAgent"),
                detail: "connection closed before acknowledgement",
              },
            },
          },
        ]}
      />,
    );

    expect(markup).toContain("Delivery uncertain");
    expect(markup).toContain("Claude may have received this message");
    expect(markup).toContain('aria-label="Retry message delivery"');
    expect(markup).toContain('aria-label="Dismiss and skip this message"');
    expect(markup.indexOf('data-user-message-body="true"')).toBeLessThan(
      markup.indexOf("Delivery uncertain"),
    );
  });

  it("uses LegendList isNearEnd when deciding whether the live edge is visible", async () => {
    const {
      resolveTimelineIsAtEnd,
      resolveTimelineMinimapHasPersistentGutter,
      resolveTimelineMinimapHeightStyle,
      resolveTimelineMinimapIndexFromPointer,
      resolveTimelineMinimapTopPercent,
    } = await import("./MessagesTimeline.logic");

    expect(resolveTimelineIsAtEnd({ isNearEnd: true, isAtEnd: false })).toBe(true);
    expect(resolveTimelineIsAtEnd({ isNearEnd: false, isAtEnd: true })).toBe(false);
    expect(resolveTimelineIsAtEnd({ isAtEnd: true })).toBe(true);
    expect(resolveTimelineIsAtEnd(undefined)).toBeUndefined();

    expect(resolveTimelineMinimapHeightStyle(5)).toBe("min(32px, calc(100vh - 18rem))");
    expect(resolveTimelineMinimapTopPercent(2, 5)).toBe(50);
    expect(
      resolveTimelineMinimapIndexFromPointer({
        itemCount: 101,
        railTop: 100,
        railHeight: 500,
        pointerY: 350,
      }),
    ).toBe(50);
    expect(
      resolveTimelineMinimapIndexFromPointer({
        itemCount: 101,
        railTop: 100,
        railHeight: 500,
        pointerY: 999,
      }),
    ).toBe(100);
    expect(resolveTimelineMinimapHasPersistentGutter(832)).toBe(false);
    expect(resolveTimelineMinimapHasPersistentGutter(863)).toBe(false);
    expect(resolveTimelineMinimapHasPersistentGutter(864)).toBe(true);
  });

  it("anchors a sent attachment message using its measured height", async () => {
    const MessagesTimeline = await loadMessagesTimeline();
    const onAnchorReady = vi.fn();
    const onAnchorSizeChanged = vi.fn();
    const firstEntry = buildUserTimelineEntry("First prompt.");
    const secondEntry = {
      ...buildUserTimelineEntry("Newest prompt."),
      id: "entry-2",
      message: {
        ...buildUserTimelineEntry("Newest prompt.").message,
        id: MessageId.make("message-2"),
        attachments: [
          {
            type: "image" as const,
            id: "attachment-1",
            name: "screenshot.png",
            mimeType: "image/png",
            sizeBytes: 1,
            previewUrl: "data:image/png;base64,iVBORw0KGgo=",
          },
        ],
      },
    };
    const markup = renderToStaticMarkup(
      <MessagesTimeline
        {...buildProps()}
        anchorMessageId={secondEntry.message.id}
        onAnchorReady={onAnchorReady}
        onAnchorSizeChanged={onAnchorSizeChanged}
        contentInsetEndAdjustment={144}
        timelineEntries={[firstEntry, secondEntry]}
      />,
    );

    expect(markup).toContain('data-anchor-index="1"');
    expect(markup).toContain('data-anchor-offset="16"');
    expect(markup).toContain('data-anchor-on-ready="true"');
    expect(markup).not.toContain("data-anchor-max-size=");
    expect(markup).toContain('data-content-inset-end="144"');
    expect(markup).toContain("[overflow-anchor:none]");
    expect(markup).not.toContain('data-maintain-scroll-at-end="enabled"');
    expect(markup).toContain('data-maintain-visible-content-position="object"');
    expect(markup).toContain('data-maintain-visible-content-position-data="true"');
    expect(markup).toContain('data-maintain-visible-content-position-size="false"');
    expect(onAnchorReady).toHaveBeenCalledOnce();
    expect(onAnchorReady).toHaveBeenCalledWith(secondEntry.message.id, 1);
    expect(onAnchorSizeChanged).toHaveBeenCalledWith(secondEntry.message.id, 240);
  });

  it("renders collapse controls for long user messages", async () => {
    const { MessagesTimeline } = await import("./MessagesTimeline");
    const markup = renderToStaticMarkup(
      <MessagesTimeline
        {...buildProps()}
        timelineEntries={[buildUserTimelineEntry(buildLongUserMessageText())]}
      />,
    );

    expect(markup).toContain("Show full message");
    expect(markup).toContain('data-maintain-scroll-at-end="enabled"');
    expect(markup).toContain('data-maintain-scroll-at-end-animated="false"');
    expect(markup).toContain('data-maintain-scroll-at-end-data-change="true"');
    expect(markup).toContain('data-maintain-scroll-at-end-item-layout="true"');
    expect(markup).toContain('data-maintain-scroll-at-end-layout="true"');
    expect(markup).toContain('data-user-message-collapsed="true"');
    expect(markup).toContain('data-user-message-fade="true"');
    expect(markup).toContain('data-user-message-footer="true"');
  });

  it("does not render collapse controls for short user messages", async () => {
    const { MessagesTimeline } = await import("./MessagesTimeline");
    const markup = renderToStaticMarkup(
      <MessagesTimeline
        {...buildProps()}
        timelineEntries={[buildUserTimelineEntry("Short prompt.")]}
      />,
    );

    expect(markup).not.toContain("Show full message");
    expect(markup).toContain('data-user-message-collapsible="false"');
  });

  it("renders inline terminal labels with the composer chip UI", async () => {
    const { MessagesTimeline } = await import("./MessagesTimeline");
    const markup = renderToStaticMarkup(
      <MessagesTimeline
        {...buildProps()}
        timelineEntries={[
          buildUserTimelineEntry(
            [
              buildLongUserMessageText("yoo what's @terminal-1:1-5 mean"),
              "",
              "<terminal_context>",
              "- Terminal 1 lines 1-5:",
              "  1 | julius@mac effect-http-ws-cli % bun i",
              "  2 | bun install v1.3.9 (cf6cdbbb)",
              "</terminal_context>",
            ].join("\n"),
          ),
        ]}
      />,
    );

    expect(markup).toContain("Terminal 1 lines 1-5");
    expect(markup).toContain("lucide-terminal");
    expect(markup).toContain("yoo what&#x27;s</p>");
    expect(markup).toContain('<span aria-hidden="true"> </span>');
    expect(markup).toContain("Show full message");
  }, 20_000);

  it("renders chips for standalone element-pick context messages", async () => {
    const { MessagesTimeline } = await import("./MessagesTimeline");
    const markup = renderToStaticMarkup(
      <MessagesTimeline
        {...buildProps()}
        timelineEntries={[
          buildUserTimelineEntry(
            [
              "<element_context>",
              "- <SubmitButton> (Button.tsx:12):",
              "  url: https://example.com/dashboard",
              "  selector: button.submit",
              "  source: /repo/src/Button.tsx:12:5",
              "  html:",
              '  <button class="submit">Save</button>',
              "</element_context>",
            ].join("\n"),
          ),
        ]}
      />,
    );

    expect(markup).toContain("SubmitButton");
    expect(markup).not.toContain("&lt;element_context");
    expect(markup).not.toContain("<element_context");
  });

  it("keeps the copy button for collapsed long user messages", async () => {
    const { MessagesTimeline } = await import("./MessagesTimeline");
    const markup = renderToStaticMarkup(
      <MessagesTimeline
        {...buildProps()}
        timelineEntries={[buildUserTimelineEntry(buildLongUserMessageText())]}
      />,
    );

    expect(markup).toContain('aria-label="Copy link"');
    expect(markup).toContain('data-user-message-collapsed="true"');
    expect(markup).toContain('data-user-message-footer="true"');
  });

  it("renders context compaction entries in the normal work log", async () => {
    const { MessagesTimeline } = await import("./MessagesTimeline");
    const markup = renderToStaticMarkup(
      <MessagesTimeline
        {...buildProps()}
        timelineEntries={[
          {
            id: "entry-1",
            kind: "work",
            createdAt: "2026-03-17T19:12:28.000Z",
            entry: {
              id: "work-1",
              createdAt: "2026-03-17T19:12:28.000Z",
              label: "Context compacted",
              tone: "info",
            },
          },
        ]}
      />,
    );

    expect(markup).toContain("Context compacted");
    expect(markup).toContain("Work Log");
  });

  it("formats changed file paths from the workspace root", async () => {
    const { MessagesTimeline } = await import("./MessagesTimeline");
    const markup = renderToStaticMarkup(
      <MessagesTimeline
        {...buildProps()}
        timelineEntries={[
          {
            id: "entry-1",
            kind: "work",
            createdAt: "2026-03-17T19:12:28.000Z",
            entry: {
              id: "work-1",
              createdAt: "2026-03-17T19:12:28.000Z",
              label: "Updated files",
              tone: "tool",
              changedFiles: ["C:/Users/mike/dev-stuff/bibcode/apps/web/src/session-logic.ts"],
            },
          },
        ]}
        workspaceRoot="C:/Users/mike/dev-stuff/bibcode"
      />,
    );

    expect(markup).toContain("bibcode/apps/web/src/session-logic.ts");
    expect(markup).not.toContain("C:/Users/mike/dev-stuff/bibcode/apps/web/src/session-logic.ts");
  });

  it("renders review comment contexts as structured cards instead of raw tags", async () => {
    const { MessagesTimeline } = await import("./MessagesTimeline");
    const markup = renderToStaticMarkup(
      <MessagesTimeline
        {...buildProps()}
        timelineEntries={[
          {
            id: "entry-1",
            kind: "message",
            createdAt: "2026-03-17T19:12:28.000Z",
            message: {
              id: MessageId.make("message-2"),
              role: "user",
              text: [
                '<review_comment sectionId="turn:2" sectionTitle="Turn 2" filePath="apps/web/src/lib/contextWindow.test.ts" startIndex="3" endIndex="14" rangeLabel="+47 to +58">',
                "Wadduo",
                "```diff",
                "@@ -0,0 +47,2 @@",
                '+  it("keeps valid zero-usage snapshots", () => {',
                "+    expect(snapshot).not.toBeNull();",
                "```",
                "</review_comment>",
              ].join("\n"),
              turnId: null,
              createdAt: "2026-03-17T19:12:28.000Z",
              updatedAt: "2026-03-17T19:12:28.000Z",
              streaming: false,
            },
          },
        ]}
      />,
    );

    expect(markup).toContain("contextWindow.test.ts");
    expect(markup).toContain("Wadduo");
    expect(markup).toContain('data-testid="file-diff"');
    expect(markup).not.toContain(">Review comment<");
    expect(markup).not.toContain("&lt;review_comment");
    expect(markup).not.toContain("&lt;/review_comment&gt;");
  });

  it("renders file review comments as source code instead of diffs", async () => {
    const { MessagesTimeline } = await import("./MessagesTimeline");
    const markup = renderToStaticMarkup(
      <MessagesTimeline
        {...buildProps()}
        timelineEntries={[
          {
            id: "entry-1",
            kind: "message",
            createdAt: "2026-03-17T19:12:28.000Z",
            message: {
              id: MessageId.make("message-source-comment"),
              role: "user",
              text: [
                '<review_comment sectionId="file:docs/plan.md" sectionTitle="File comment" filePath="docs/plan.md" startIndex="0" endIndex="1" rangeLabel="L1 to L2">',
                "Clarify this.",
                "```md",
                "# Plan",
                "- Step one",
                "```",
                "</review_comment>",
              ].join("\n"),
              turnId: null,
              createdAt: "2026-03-17T19:12:28.000Z",
              updatedAt: "2026-03-17T19:12:28.000Z",
              streaming: false,
            },
          },
        ]}
      />,
    );

    expect(markup).toContain("plan.md");
    expect(markup).toContain("Clarify this.");
    expect(markup).toContain("# Plan");
    expect(markup).not.toContain('data-testid="file-diff"');
  });

  it("renders the empty state when there are no rows and no work in flight", async () => {
    const { MessagesTimeline } = await import("./MessagesTimeline");
    const markup = renderToStaticMarkup(
      <MessagesTimeline {...buildProps()} timelineEntries={[]} />,
    );

    expect(markup).toContain("Send a message to start the conversation.");
    expect(markup).not.toContain("legend-list");
  });

  it("renders a zeroed waiting timer when the active turn start is unknown", async () => {
    const { MessagesTimeline } = await import("./MessagesTimeline");
    const markup = renderToStaticMarkup(
      <MessagesTimeline {...buildProps()} isWorking timelineEntries={[]} />,
    );

    expect(markup).toContain("Waiting for");
    expect(markup).toContain(">0s<");
  });

  it("renders the reversed dotted-square before a whole-second waiting timer", async () => {
    const { MessagesTimeline } = await import("./MessagesTimeline");
    const startedAt = new Date(Date.now() - 30_000).toISOString();
    const markup = renderToStaticMarkup(
      <MessagesTimeline
        {...buildProps()}
        isWorking
        activeTurnStartedAt={startedAt}
        timelineEntries={[]}
      />,
    );

    expect(markup).toContain('data-working-indicator="reversed"');
    expect(markup.match(/data-working-indicator-dot=/g) ?? []).toHaveLength(8);
    expect(markup.indexOf('data-working-indicator="reversed"')).toBeLessThan(
      markup.indexOf("Waiting for"),
    );
    expect(markup).toContain('style="animation-delay:-980ms"');
    expect(markup).toMatch(/Waiting for <span[^>]*>(29|30|31)s</);
    expect(markup).not.toContain("Working for");
  });

  it("formats the working timer with minutes and seconds", async () => {
    const { MessagesTimeline } = await import("./MessagesTimeline");
    const startedAt = new Date(Date.now() - 90_000).toISOString();
    const markup = renderToStaticMarkup(
      <MessagesTimeline
        {...buildProps()}
        isWorking
        activeTurnStartedAt={startedAt}
        timelineEntries={[]}
      />,
    );

    expect(markup).toMatch(/1m (29|30|31)s/);
  });

  it("formats the working timer with hours, minutes, and whole seconds", async () => {
    const { MessagesTimeline } = await import("./MessagesTimeline");
    const startedAt = new Date(Date.now() - 3_690_000).toISOString();
    const markup = renderToStaticMarkup(
      <MessagesTimeline
        {...buildProps()}
        isWorking
        activeTurnStartedAt={startedAt}
        timelineEntries={[]}
      />,
    );

    expect(markup).toMatch(/1h 1m (29|30|31)s/);
  });

  it("retains zero minutes when an hour-scale timer includes seconds", async () => {
    const { MessagesTimeline } = await import("./MessagesTimeline");
    const startedAt = new Date(Date.now() - 3_650_000).toISOString();
    const markup = renderToStaticMarkup(
      <MessagesTimeline
        {...buildProps()}
        isWorking
        activeTurnStartedAt={startedAt}
        timelineEntries={[]}
      />,
    );

    expect(markup).toMatch(/1h 0m (49|50|51)s/);
  });

  it("stops scheduling waiting timer paints after unmount", async () => {
    const animationFrameHarness = installAnimationFrameHarness();
    await mountTimeline({
      isWorking: true,
      activeTurnStartedAt: new Date(Date.now() - 3_800).toISOString(),
      timelineEntries: [],
    });

    expect(animationFrameHarness.pendingCount()).toBeGreaterThan(0);
    const mounted = mountedTrees.pop()!;
    await act(async () => mounted.root.unmount());
    mounted.container.remove();
    expect(animationFrameHarness.pendingCount()).toBe(0);
  });

  it("advances the waiting timer on the next whole second when the source clock is ahead", async () => {
    const animationFrameHarness = installAnimationFrameHarness();

    const container = await mountTimeline({
      isWorking: true,
      activeTurnStartedAt: new Date(Date.now() + 2_000).toISOString(),
      timelineEntries: [],
    });
    expect(container.textContent).toContain("Waiting for 0s");

    await animationFrameHarness.advanceBy(120);
    expect(container.textContent).toContain("Waiting for 0s");

    await animationFrameHarness.advanceBy(900);

    expect(container.textContent).toContain("Waiting for 1s");
  });

  it("starts advancing immediately while the turn start timestamp is still unknown", async () => {
    const animationFrameHarness = installAnimationFrameHarness();

    const container = await mountTimeline({
      isWorking: true,
      activeTurnStartedAt: null,
      timelineEntries: [],
    });
    expect(container.textContent).toContain("Waiting for 0s");

    await animationFrameHarness.advanceBy(120);
    expect(container.textContent).toContain("Waiting for 0s");

    await animationFrameHarness.advanceBy(900);

    expect(container.textContent).toContain("Waiting for 1s");
  });

  it("does not move backward when the server replaces the local timer start", async () => {
    const animationFrameHarness = installAnimationFrameHarness();
    const MessagesTimeline = await loadMessagesTimeline();
    const container = await mountTimeline({
      isWorking: true,
      activeTurnStartedAt: null,
      timelineEntries: [],
    });
    await animationFrameHarness.advanceBy(1_250);
    expect(container.textContent).toContain("Waiting for 1s");

    const mounted = mountedTrees.at(-1)!;
    await act(async () =>
      mounted.root.render(
        <MessagesTimeline
          {...buildProps()}
          isWorking
          activeTurnStartedAt={new Date().toISOString()}
          timelineEntries={[]}
        />,
      ),
    );
    expect(container.textContent).toContain("Waiting for 1s");

    await animationFrameHarness.advanceBy(800);
    expect(container.textContent).toContain("Waiting for 2s");
  });

  it("renders a failure marker for failed tool lifecycle entries", async () => {
    const { MessagesTimeline } = await import("./MessagesTimeline");
    const markup = renderToStaticMarkup(
      <MessagesTimeline
        {...buildProps()}
        timelineEntries={[
          {
            id: "entry-1",
            kind: "work",
            createdAt: "2026-03-17T19:12:28.000Z",
            entry: {
              id: "work-1",
              createdAt: "2026-03-17T19:12:28.000Z",
              label: "Glob",
              tone: "tool",
              toolLifecycleStatus: "failed",
              detail: "No files found",
            },
          },
        ]}
      />,
    );

    expect(markup).toContain("lucide-x");
    expect(markup).toContain('aria-label="Tool call failed"');
  });
});

const TURN_ID = TurnId.make("turn-1");
const LATER_CREATED_AT = "2026-03-17T19:12:33.000Z";

function buildAssistantTimelineEntry(overrides: {
  id: string;
  messageId: string;
  text: string;
  turnId?: ReturnType<typeof TurnId.make> | null;
  streaming?: boolean;
  createdAt?: string;
}) {
  const createdAt = overrides.createdAt ?? LATER_CREATED_AT;
  return {
    id: overrides.id,
    kind: "message" as const,
    createdAt,
    message: {
      id: MessageId.make(overrides.messageId),
      role: "assistant" as const,
      text: overrides.text,
      turnId: overrides.turnId ?? null,
      createdAt,
      updatedAt: createdAt,
      streaming: overrides.streaming ?? false,
    },
  };
}

function buildWorkTimelineEntry(id: string, entry: Partial<WorkLogEntry> = {}) {
  return {
    id,
    kind: "work" as const,
    createdAt: MESSAGE_CREATED_AT,
    entry: {
      id: `${id}-entry`,
      createdAt: MESSAGE_CREATED_AT,
      label: "Tool",
      tone: "tool" as const,
      ...entry,
    },
  };
}

type MessagesTimelineProps = ComponentProps<typeof import("./MessagesTimeline").MessagesTimeline>;

async function renderTimeline(
  props: Partial<MessagesTimelineProps> & Pick<MessagesTimelineProps, "timelineEntries">,
) {
  const MessagesTimeline = await loadMessagesTimeline();
  h.legendListProps = null;
  return renderToStaticMarkup(<MessagesTimeline {...buildProps()} {...props} />);
}

async function mountTimeline(
  props: Partial<MessagesTimelineProps> & Pick<MessagesTimelineProps, "timelineEntries">,
): Promise<HTMLDivElement> {
  const MessagesTimeline = await loadMessagesTimeline();
  h.legendListProps = null;
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mountedTrees.push({ container, root });
  await act(async () => root.render(<MessagesTimeline {...buildProps()} {...props} />));
  return container;
}

function installAnimationFrameHarness(initialNowMs = 1_000) {
  const animationFrames = new Map<number, FrameRequestCallback>();
  let monotonicNowMs = initialNowMs;
  let nextAnimationFrameId = 0;
  vi.spyOn(globalThis.performance, "now").mockImplementation(() => monotonicNowMs);
  vi.stubGlobal(
    "requestAnimationFrame",
    vi.fn((callback: FrameRequestCallback) => {
      const id = ++nextAnimationFrameId;
      animationFrames.set(id, callback);
      return id;
    }),
  );
  vi.stubGlobal(
    "cancelAnimationFrame",
    vi.fn((id: number) => animationFrames.delete(id)),
  );

  return {
    pendingCount: () => animationFrames.size,
    advanceBy: async (elapsedMs: number) => {
      monotonicNowMs += elapsedMs;
      const callbacks = [...animationFrames.values()];
      animationFrames.clear();
      await act(async () => callbacks.forEach((callback) => callback(monotonicNowMs)));
    },
  };
}

async function click(element: HTMLElement): Promise<void> {
  await act(async () => element.click());
}

describe("MessagesTimeline assistant rows", () => {
  it("renders terminal assistant messages with metadata and copy button", async () => {
    const markup = await renderTimeline({
      timelineEntries: [
        buildUserTimelineEntry("Explain this."),
        buildAssistantTimelineEntry({
          id: "entry-2",
          messageId: "message-assistant-1",
          text: "Here is the answer.",
          turnId: TURN_ID,
        }),
      ],
      latestTurn: {
        turnId: TURN_ID,
        state: "completed",
        startedAt: MESSAGE_CREATED_AT,
        completedAt: LATER_CREATED_AT,
      },
    });

    expect(markup).toContain("Here is the answer.");
    expect(markup).toContain("group/assistant");
    expect(markup).toContain('aria-label="Copy link"');
    expect(markup).toContain('data-message-role="assistant"');
  });

  it("renders a placeholder for empty settled assistant responses", async () => {
    const markup = await renderTimeline({
      timelineEntries: [
        buildAssistantTimelineEntry({
          id: "entry-1",
          messageId: "message-assistant-empty",
          text: "",
        }),
      ],
    });

    expect(markup).toContain("(empty response)");
  });

  it("does not render a placeholder while an assistant message is streaming", async () => {
    const markup = await renderTimeline({
      timelineEntries: [
        buildAssistantTimelineEntry({
          id: "entry-1",
          messageId: "message-assistant-streaming",
          text: "",
          streaming: true,
        }),
      ],
    });

    expect(markup).not.toContain("(empty response)");
  });

  it("renders the changed files summary for assistant turn diffs", async () => {
    const assistantMessageId = MessageId.make("message-assistant-diff");
    const markup = await renderTimeline({
      timelineEntries: [
        buildUserTimelineEntry("Change files."),
        buildAssistantTimelineEntry({
          id: "entry-2",
          messageId: "message-assistant-diff",
          text: "Changed two files.",
          turnId: TURN_ID,
        }),
      ],
      turnDiffSummaryByAssistantMessageId: new Map([
        [
          assistantMessageId,
          {
            turnId: TURN_ID,
            checkpointTurnCount: 1,
            checkpointRef: CheckpointRef.make("checkpoint-1"),
            status: "ready" as const,
            files: [
              { path: "src/alpha.ts", kind: "modified", additions: 3, deletions: 1 },
              { path: "src/beta.ts", kind: "added", additions: 10, deletions: 0 },
            ],
            assistantMessageId,
            completedAt: LATER_CREATED_AT,
          },
        ],
      ]),
    });

    expect(markup).toContain("Changed files (2)");
    expect(markup).toContain("Collapse all");
    expect(markup).toContain("View diff");
    expect(markup).toContain("alpha.ts");
    expect(markup).toContain("beta.ts");
  });
});

describe("MessagesTimeline turn folds", () => {
  const foldableEntries = () => [
    buildUserTimelineEntry("Do the work."),
    buildWorkTimelineEntry("entry-work", {
      turnId: TURN_ID,
      label: "Read file",
      command: "cat notes.txt",
    }),
    buildAssistantTimelineEntry({
      id: "entry-commentary",
      messageId: "message-commentary",
      text: "Intermediate commentary.",
      turnId: TURN_ID,
      createdAt: "2026-03-17T19:12:30.000Z",
    }),
    buildAssistantTimelineEntry({
      id: "entry-terminal",
      messageId: "message-terminal",
      text: "All done now.",
      turnId: TURN_ID,
    }),
  ];

  it("folds settled turns behind a Worked-for row keeping the terminal message", async () => {
    const markup = await renderTimeline({
      timelineEntries: foldableEntries(),
      latestTurn: {
        turnId: TURN_ID,
        state: "completed",
        startedAt: MESSAGE_CREATED_AT,
        completedAt: LATER_CREATED_AT,
      },
    });

    expect(markup).toContain("Worked for 5.0s");
    expect(markup).toContain('aria-expanded="false"');
    expect(markup).toContain("All done now.");
    expect(markup).not.toContain("Intermediate commentary.");
    expect(markup).not.toContain("cat notes.txt");
  });

  it("labels interrupted turns as stopped by the user", async () => {
    const markup = await renderTimeline({
      timelineEntries: foldableEntries(),
      latestTurn: {
        turnId: TURN_ID,
        state: "interrupted",
        startedAt: MESSAGE_CREATED_AT,
        completedAt: LATER_CREATED_AT,
      },
    });

    expect(markup).toContain("You stopped after 5.0s");
  });

  it("keeps running turns unfolded", async () => {
    const markup = await renderTimeline({
      timelineEntries: foldableEntries(),
      runningTurnId: TURN_ID,
      latestTurn: {
        turnId: TURN_ID,
        state: "running",
        startedAt: MESSAGE_CREATED_AT,
        completedAt: null,
      },
    });

    expect(markup).not.toContain("Worked for");
    expect(markup).toContain("Intermediate commentary.");
    expect(markup).toContain("All done now.");
  });
});

describe("MessagesTimeline work groups", () => {
  it("collapses long tool call runs behind a work-toggle row", async () => {
    const markup = await renderTimeline({
      timelineEntries: [
        buildWorkTimelineEntry("work-1", { label: "First", command: "echo one" }),
        buildWorkTimelineEntry("work-2", { label: "Second", command: "echo two" }),
        buildWorkTimelineEntry("work-3", { label: "Third", command: "echo three" }),
      ],
    });

    expect(markup).toContain("+2 previous tool calls");
    expect(markup).toContain('aria-expanded="false"');
    expect(markup).toContain("echo three");
    expect(markup).not.toContain("echo one");
  });

  it.each([
    {
      tone: "tool",
      hiddenCount: 1,
      label: "+1 previous tool call",
      expandedLabel: "Show fewer tool calls",
    },
    {
      tone: "tool",
      hiddenCount: 2,
      label: "+2 previous tool calls",
      expandedLabel: "Show fewer tool calls",
    },
    {
      tone: "info",
      hiddenCount: 1,
      label: "+1 previous log entry",
      expandedLabel: "Show fewer log entries",
    },
    {
      tone: "info",
      hiddenCount: 2,
      label: "+2 previous log entries",
      expandedLabel: "Show fewer log entries",
    },
  ] as const)(
    "labels the work toggle '$label' with text-xs",
    async ({ tone, hiddenCount, label, expandedLabel }) => {
      const container = await mountTimeline({
        timelineEntries: Array.from({ length: hiddenCount + 1 }, (_, index) =>
          buildWorkTimelineEntry(`work-${index}`, { label: `Entry ${index}`, tone }),
        ),
      });
      const toggle = container.querySelector<HTMLButtonElement>("button[aria-expanded]");

      expect.soft(toggle?.textContent).toBe(label);
      expect.soft(toggle?.classList.contains("text-xs")).toBe(true);
      expect(toggle).not.toBeNull();
      await click(toggle!);
      expect(toggle?.textContent).toBe(expandedLabel);
      expect(container.textContent).toContain("Entry 0");
    },
  );
});

describe("MessagesTimeline work entry rows", () => {
  async function renderWorkEntry(entry: Parameters<typeof buildWorkTimelineEntry>[1]) {
    return renderTimeline({ timelineEntries: [buildWorkTimelineEntry("work-1", entry)] });
  }

  it.each([
    { label: "Turn completed", tone: "info", sourceActivityKind: "provider.turn" },
    { label: "Step completed", tone: "info", sourceActivityKind: "provider.event" },
    { label: "Step completed", tone: "tool", sourceActivityKind: "provider.event" },
    { label: "Read completed", tone: "tool", sourceActivityKind: "provider.event" },
  ] as const)("keeps the full '$label' provider heading", async (entry) => {
    const markup = await renderWorkEntry(entry);

    expect(markup).toContain(`>${entry.label}</span>`);
  });

  it.each(["tool", "info"] as const)(
    "normalizes %s-toned tool lifecycle labels without a tool title",
    async (tone) => {
      const markup = await renderWorkEntry({
        label: "Read file completed",
        tone,
        sourceActivityKind: "tool.completed",
      });

      expect(markup).toContain(">Read file</span>");
      expect(markup).not.toContain("Read file completed");
    },
  );

  it("renders command entries with the terminal icon and success indicator", async () => {
    const markup = await renderWorkEntry({
      label: "Run command",
      command: "pnpm test",
      toolLifecycleStatus: "completed",
    });

    expect(markup).toContain("lucide-terminal");
    expect(markup).toContain("lucide-check");
    expect(markup).toContain("pnpm test");
    expect(markup).toContain('role="button"');
  });

  it("renders file-read approval entries with the eye icon", async () => {
    const markup = await renderWorkEntry({ label: "Read", requestKind: "file-read" });

    expect(markup).toContain("lucide-eye");
  });

  it("renders file-change approval entries with the pen icon", async () => {
    const markup = await renderWorkEntry({ label: "Edit", requestKind: "file-change" });

    expect(markup).toContain("lucide-square-pen");
  });

  it("renders command approval entries with the terminal icon", async () => {
    const markup = await renderWorkEntry({ label: "Approve", requestKind: "command" });

    expect(markup).toContain("lucide-terminal");
  });

  it("renders MCP tool calls with the wrench icon and expandable payload", async () => {
    const markup = await renderWorkEntry({
      label: "MCP tool",
      itemType: "mcp_tool_call",
      toolData: { server: "docs", args: { q: "hello" } },
    });

    expect(markup).toContain("lucide-wrench");
    expect(markup).toContain('role="button"');
  });

  it("renders web searches with the globe icon", async () => {
    const markup = await renderWorkEntry({
      label: "Search web",
      itemType: "web_search",
      detail: "searched the docs",
    });

    expect(markup).toContain("lucide-globe");
  });

  it("renders image views with the eye icon", async () => {
    const markup = await renderWorkEntry({
      label: "View image",
      itemType: "image_view",
      detail: "screenshot.png",
    });

    expect(markup).toContain("lucide-eye");
  });

  it("renders dynamic tool calls with the hammer icon", async () => {
    const markup = await renderWorkEntry({
      label: "Dyn tool",
      itemType: "dynamic_tool_call",
      detail: "ran helper",
    });

    expect(markup).toContain("lucide-hammer");
  });

  it("renders user-input requests with the message icon", async () => {
    const markup = await renderWorkEntry({
      label: "Question",
      detail: "Which option?",
      sourceActivityKind: "user-input.requested",
    });

    expect(markup).toContain("lucide-message-circle");
  });

  it("renders provider warnings with an amber alert icon and normal text", async () => {
    const markup = await renderWorkEntry({
      label: "Codex warning",
      detail: "Rate limited; retrying",
      tone: "warning",
      sourceActivityKind: "provider.warning",
    });

    expect(markup).toContain("lucide-circle-alert");
    expect(markup).toContain("text-warning-foreground");
    expect(markup).toContain("font-medium text-foreground/82");
    expect(markup).not.toContain("lucide-x");
    expect(markup).not.toContain("text-destructive");
    expect(markup).not.toContain('aria-label="Tool call failed"');
  });

  it("renders runtime errors with destructive chrome", async () => {
    const markup = await renderWorkEntry({
      label: "Crashed",
      detail: "boom",
      tone: "error",
      sourceActivityKind: "runtime.error",
    });

    expect(markup).toContain("text-destructive");
    expect(markup).toContain('aria-label="Tool call failed"');
  });

  it("renders failed thinking entries with the bot icon", async () => {
    const markup = await renderWorkEntry({
      label: "Pondering",
      tone: "thinking",
      toolLifecycleStatus: "failed",
    });

    expect(markup).toContain("lucide-bot");
  });

  it("summarizes multiple changed files in the preview", async () => {
    const markup = await renderWorkEntry({
      label: "Updated files",
      changedFiles: ["/repo/src/a.ts", "/repo/src/b.ts", "/repo/src/c.ts"],
      toolLifecycleStatus: "completed",
    });

    expect(markup).toContain("lucide-square-pen");
    expect(markup).toContain("+2 more");
  });

  it("prefers the tool title over the label and strips completed suffixes", async () => {
    const markup = await renderWorkEntry({
      label: "tool call completed",
      toolTitle: "search files completed",
      detail: "3 matches",
      toolLifecycleStatus: "completed",
    });

    expect(markup).toContain(">Search files</span>");
    expect(markup).not.toContain("Search files completed");
    expect(markup).not.toContain("Tool call completed");
    expect(markup).toContain("3 matches");
  });

  it.each(["search files", "SEARCH FILES complete", "search files completed"])(
    "drops a tool preview '%s' that repeats the normalized heading",
    async (detail) => {
      const markup = await renderWorkEntry({
        label: "Tool call completed",
        toolTitle: "search files completed",
        detail,
        toolLifecycleStatus: "completed",
      });

      expect(markup).toContain(">Search files</span>");
      expect(markup).not.toContain(`>${detail}</span>`);
    },
  );
});

describe("MessagesTimeline user message affordances", () => {
  it("shows a local pending upload on its message with a focusable Cancel action", async () => {
    const entry = buildUserTimelineEntry("Outgoing files");
    const cancel = vi.fn();
    const container = await mountTimeline({
      timelineEntries: [entry],
      attachmentUploads: {
        [entry.message.id]: {
          attachmentCount: 2,
          fileName: "a.png",
          sentBytes: 3.1 * 1024 ** 2,
          totalBytes: 20 * 1024 ** 2,
          phase: "uploading",
        },
      },
      onCancelAttachmentUpload: cancel,
    });
    expect(container.textContent).toContain("Uploading 2 attachments — 3.1 of 20 MiB");
    const button = [...container.querySelectorAll("button")].find(
      (value) => value.textContent === "Cancel",
    )!;
    button.focus();
    expect(document.activeElement).toBe(button);
    await click(button);
    expect(cancel).toHaveBeenCalledExactlyOnceWith(entry.message.id);
    expect(container.textContent).not.toContain("Queued");
  });
  it("renders the revert button when the message has revertable turns", async () => {
    const entry = buildUserTimelineEntry("Revert me.");
    const markup = await renderTimeline({
      timelineEntries: [entry],
      revertTurnCountByUserMessageId: new Map([[entry.message.id, 2]]),
    });

    expect(markup).toContain('aria-label="Revert to this message"');
    expect(markup).toContain("lucide-undo-2");
  });

  it("disables the revert button while a checkpoint revert is running", async () => {
    const entry = buildUserTimelineEntry("Revert me.");
    const markup = await renderTimeline({
      timelineEntries: [entry],
      revertTurnCountByUserMessageId: new Map([[entry.message.id, 1]]),
      isRevertingCheckpoint: true,
    });

    expect(markup).toContain('aria-label="Revert to this message"');
    expect(markup).toContain("disabled");
  });

  it("renders preview annotation cards with comment, targets and style changes", async () => {
    const text = [
      "Make the button blue",
      "",
      "<preview_annotation>",
      "Preview annotation:",
      "Id: anno-1",
      "Page: Dashboard",
      "Comment: Blue please",
      "Targets: 1 selected element.",
      "Requested visual changes:",
      "- color: red → blue",
      "The attached screenshot is the annotated preview crop.",
      "</preview_annotation>",
    ].join("\n");
    const entry = {
      ...buildUserTimelineEntry(text),
      message: {
        ...buildUserTimelineEntry(text).message,
        attachments: [
          {
            type: "image" as const,
            id: "attachment-annotation",
            name: "preview-annotation-anno-1.png",
            mimeType: "image/png",
            sizeBytes: 1,
            previewUrl: "data:image/png;base64,iVBORw0KGgo=",
          },
        ],
      },
    };
    const markup = await renderTimeline({ timelineEntries: [entry] });

    expect(markup).toContain("Blue please");
    expect(markup).toContain("1 selected element.");
    expect(markup).toContain("lucide-paintbrush");
    expect(markup).toContain("Annotated preview crop");
    expect(markup).toContain("Make the button blue");
    expect(markup).not.toContain("<preview_annotation>");
  });
});

describe("MessagesTimeline row typing and scroll wiring", () => {
  it("derives distinct item types per row kind", async () => {
    const markup = await renderTimeline({
      isWorking: true,
      timelineEntries: [
        buildUserTimelineEntry("A question"),
        buildAssistantTimelineEntry({
          id: "entry-2",
          messageId: "message-assistant-types",
          text: "An answer",
        }),
        buildWorkTimelineEntry("entry-3", { label: "Tooling", command: "ls" }),
      ],
    });

    expect(markup).toContain('data-item-type="message:user"');
    expect(markup).toContain('data-item-type="message:assistant"');
    expect(markup).toContain('data-item-type="work"');
    expect(markup).toContain('data-item-type="working"');
  });

  it("reports the live-edge state from the list scroll state", async () => {
    const onIsAtEndChange = vi.fn();
    const listRef = createRef<LegendListRef | null>();
    listRef.current = {
      getState: () => ({
        isNearEnd: true,
        scroll: 0,
        scrollLength: 400,
        positionAtIndex: () => 10,
        sizeAtIndex: () => 40,
      }),
    } as unknown as LegendListRef;

    const secondUser = {
      ...buildUserTimelineEntry("Second question"),
      id: "entry-2",
      message: {
        ...buildUserTimelineEntry("Second question").message,
        id: MessageId.make("message-user-scroll-2"),
      },
    };
    await renderTimeline({
      listRef,
      onIsAtEndChange,
      timelineEntries: [buildUserTimelineEntry("First question"), secondUser],
    });

    expect(onIsAtEndChange).toHaveBeenCalledWith(true);
  });
});

describe("MessagesTimeline proposed plans", () => {
  it("renders proposed plan rows through the plan card", async () => {
    const markup = await renderTimeline({
      timelineEntries: [
        {
          id: "entry-plan",
          kind: "proposed-plan" as const,
          createdAt: MESSAGE_CREATED_AT,
          proposedPlan: {
            id: "plan-1",
            turnId: TURN_ID,
            planMarkdown: "Follow the plan steps carefully.",
            implementedAt: null,
            implementationThreadId: null,
            createdAt: MESSAGE_CREATED_AT,
            updatedAt: MESSAGE_CREATED_AT,
          },
        },
      ],
    });

    expect(markup).toContain('data-item-type="proposed-plan"');
    expect(markup).toContain("Follow the plan steps carefully.");
  });
});

describe("MessagesTimeline terminal context bodies", () => {
  const terminalContextBlock = [
    "<terminal_context>",
    "- Terminal 1 lines 1-5:",
    "  1 | pnpm install",
    "  2 | done",
    "</terminal_context>",
  ].join("\n");

  it("renders context-only user messages as bare chips", async () => {
    const markup = await renderTimeline({
      timelineEntries: [buildUserTimelineEntry(terminalContextBlock)],
    });

    expect(markup).toContain("Terminal 1 lines 1-5");
    expect(markup).toContain("lucide-terminal");
    expect(markup).toContain('data-user-message-collapsible="false"');
  });

  it("prefixes chips before prose that does not mention the labels inline", async () => {
    const markup = await renderTimeline({
      timelineEntries: [buildUserTimelineEntry(`what went wrong here?\n\n${terminalContextBlock}`)],
    });

    expect(markup).toContain("Terminal 1 lines 1-5");
    expect(markup).toContain("what went wrong here?");
    expect(markup).toContain('aria-hidden="true"> </span>');
  });
});

describe("MessagesTimeline working timer resilience", () => {
  it("falls back to a zero timer when the turn start timestamp is invalid", async () => {
    const markup = await renderTimeline({
      isWorking: true,
      activeTurnStartedAt: "not-a-timestamp",
      timelineEntries: [],
    });

    expect(markup).toContain("Waiting for");
    expect(markup).toContain(">0s<");
  });
});

describe("MessagesTimeline work entry expanded bodies", () => {
  it("keeps rows expandable when the raw command differs from the display command", async () => {
    const markup = await renderTimeline({
      timelineEntries: [
        buildWorkTimelineEntry("work-raw", {
          label: "Run",
          command: "ls",
          rawCommand: "ls --color=auto",
        }),
      ],
    });

    expect(markup).toContain('role="button"');
    expect(markup).toContain("lucide-chevron-down");
  });

  it("treats identical raw and display commands as a single command body", async () => {
    const markup = await renderTimeline({
      timelineEntries: [
        buildWorkTimelineEntry("work-same", {
          label: "Run",
          command: "pwd",
          rawCommand: "pwd",
        }),
      ],
    });

    expect(markup).toContain('role="button"');
    expect(markup).toContain("pwd");
  });

  it("tolerates empty labels", async () => {
    const markup = await renderTimeline({
      timelineEntries: [
        buildWorkTimelineEntry("work-empty-label", {
          label: "",
          command: "true",
        }),
      ],
    });

    expect(markup).toContain("lucide-terminal");
    expect(markup).toContain("true");
  });
});

describe("MessagesTimeline minimap", () => {
  it("renders minimap strips for each user message once past the threshold", async () => {
    const secondUser = {
      ...buildUserTimelineEntry("Second question"),
      id: "entry-3",
      message: {
        ...buildUserTimelineEntry("Second question").message,
        id: MessageId.make("message-user-2"),
      },
    };
    const markup = await renderTimeline({
      timelineEntries: [
        buildUserTimelineEntry("First question"),
        buildAssistantTimelineEntry({
          id: "entry-2",
          messageId: "message-assistant-mm",
          text: "First answer",
        }),
        secondUser,
      ],
    });

    expect(markup).toContain('data-testid="timeline-minimap"');
    expect(markup).toContain('data-persistent-gutter="false"');
    expect(markup).toContain('aria-label="Jump to message: User message"');
    expect(markup.match(/data-minimap-strip/g)?.length).toBe(2);
    expect(markup).toContain("min(8px, calc(100vh - 18rem))");
  });

  it("does not render the minimap below the item threshold", async () => {
    const markup = await renderTimeline({
      timelineEntries: [buildUserTimelineEntry("Only question")],
    });

    expect(markup).not.toContain('data-testid="timeline-minimap"');
  });

  it("drives pointer, keyboard, scroll, and strip visibility behavior", async () => {
    const onManualNavigation = vi.fn();
    const onIsAtEndChange = vi.fn();
    const scrollToIndex = vi.fn();
    const state = {
      isNearEnd: false,
      scroll: 10,
      scrollLength: 100,
      positionAtIndex: (index: number) => (index === 0 ? 0 : 220),
      sizeAtIndex: (index: number) => (index === 0 ? 40 : Number.NaN),
    };
    const listRef = {
      current: {
        getState: () => state,
        scrollToIndex,
      },
    } as unknown as React.RefObject<LegendListRef | null>;
    const secondUser = {
      ...buildUserTimelineEntry("Second question"),
      id: "entry-user-2",
      message: {
        ...buildUserTimelineEntry("Second question").message,
        id: MessageId.make("message-user-2"),
      },
    };

    const container = await mountTimeline({
      listRef,
      onManualNavigation,
      onIsAtEndChange,
      contentInsetEndAdjustment: -4,
      timelineEntries: [
        buildUserTimelineEntry("First question"),
        buildAssistantTimelineEntry({
          id: "entry-assistant-1",
          messageId: "message-assistant-1",
          text: "First answer",
        }),
        secondUser,
      ],
    });

    const strips = Array.from(container.querySelectorAll<HTMLElement>("[data-minimap-strip]"));
    await act(async () => {
      const onScroll = h.legendListProps?.onScroll;
      if (typeof onScroll === "function") onScroll();
    });

    expect(onIsAtEndChange).toHaveBeenCalledWith(false);
    expect(strips[0]?.dataset.inView).toBe("true");
    expect(strips[1]?.dataset.inView).toBe("false");

    const minimap = container.querySelector<HTMLButtonElement>(
      'button[aria-label^="Jump to message:"]',
    );
    expect(minimap).not.toBeNull();
    vi.spyOn(minimap!, "getBoundingClientRect").mockReturnValue({
      top: 100,
      height: 100,
    } as DOMRect);

    await act(async () => minimap!.focus());
    expect(minimap?.getAttribute("aria-label")).toContain("First question");

    const arrowDown = new KeyboardEvent("keydown", {
      key: "ArrowDown",
      bubbles: true,
      cancelable: true,
    });
    await act(async () => minimap!.dispatchEvent(arrowDown));
    expect(arrowDown.defaultPrevented).toBe(true);
    expect(minimap?.getAttribute("aria-label")).toContain("Second question");

    for (const key of ["Enter", " "]) {
      const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true });
      await act(async () => minimap!.dispatchEvent(event));
      expect(event.defaultPrevented).toBe(true);
    }

    const mouseDown = new MouseEvent("mousedown", {
      clientY: 190,
      bubbles: true,
      cancelable: true,
    });
    await act(async () => minimap!.dispatchEvent(mouseDown));
    expect(mouseDown.defaultPrevented).toBe(true);
    await act(async () =>
      minimap!.dispatchEvent(
        new MouseEvent("click", { clientY: 190, bubbles: true, cancelable: true }),
      ),
    );

    expect(onManualNavigation).toHaveBeenCalledTimes(3);
    expect(scrollToIndex).toHaveBeenCalledWith({ index: 2, animated: true, viewOffset: 24 });
    expect(document.activeElement).not.toBe(minimap);
  });
});

describe("MessagesTimeline mounted interactions", () => {
  it("scopes retry and dismiss actions to the affected message", async () => {
    const onResolveTurnDelivery = vi.fn();
    const entry = buildUserTimelineEntry("Send this once.");
    const timelineEntry = {
      ...entry,
      message: {
        ...entry.message,
        delivery: {
          state: "failed" as const,
          provider: ProviderDriverKind.make("codex"),
          detail: "provider rejected the request",
        },
      },
    };
    const container = await mountTimeline({
      timelineEntries: [timelineEntry],
      onResolveTurnDelivery,
    });

    const retry = container.querySelector<HTMLElement>('[aria-label="Retry message delivery"]');
    const dismiss = container.querySelector<HTMLElement>(
      '[aria-label="Dismiss and skip this message"]',
    );
    expect(retry).not.toBeNull();
    expect(dismiss).not.toBeNull();
    await click(retry!);
    await click(dismiss!);

    expect(onResolveTurnDelivery.mock.calls).toEqual([
      [entry.message.id, "retry"],
      [entry.message.id, "dismiss"],
    ]);
  });

  it("expands regular image attachments and reverts from their user message", async () => {
    const onImageExpand = vi.fn();
    const onRevertUserMessage = vi.fn();
    const entry = buildUserTimelineEntry("Review these screenshots");
    const messageId = entry.message.id;
    const timelineEntry = {
      ...entry,
      message: {
        ...entry.message,
        attachments: [
          {
            type: "image" as const,
            id: "image-previewable",
            name: "screen.png",
            mimeType: "image/png",
            sizeBytes: 12,
            previewUrl: "data:image/png;base64,AA==",
          },
          {
            type: "image" as const,
            id: "image-without-preview",
            name: "missing.png",
            mimeType: "image/png",
            sizeBytes: 0,
            previewUrl: "",
          },
        ],
      },
    };

    const container = await mountTimeline({
      timelineEntries: [timelineEntry],
      revertTurnCountByUserMessageId: new Map([[messageId, 0]]),
      onImageExpand,
      onRevertUserMessage,
    });
    const preview = container.querySelector<HTMLButtonElement>(
      'button[aria-label="Preview screen.png"]',
    );
    const revert = container.querySelector<HTMLButtonElement>(
      'button[aria-label="Revert to this message"]',
    );
    expect(preview).not.toBeNull();
    expect(revert).not.toBeNull();

    await click(preview!);
    await click(revert!);

    expect(container.textContent).toContain("missing.png");
    expect(onImageExpand).toHaveBeenCalledWith(
      expect.objectContaining({
        index: 0,
        images: [{ name: "screen.png", src: "data:image/png;base64,AA==" }],
      }),
    );
    expect(onRevertUserMessage).toHaveBeenCalledWith(messageId);
  });

  it("expands a folded turn and opens its rendered diff action", async () => {
    const onOpenTurnDiff = vi.fn();
    const assistantMessageId = MessageId.make("message-assistant-actions");
    const container = await mountTimeline({
      timelineEntries: [
        buildUserTimelineEntry("Run it"),
        buildWorkTimelineEntry("work-action", {
          turnId: TURN_ID,
          label: "MCP call completed",
          itemType: "mcp_tool_call",
          command: "display command",
          rawCommand: "raw command --verbose",
          detail: "Returned data",
          changedFiles: ["/repo/src/a.ts", "/repo/src/b.ts"],
          toolData: { query: "status" },
        }),
        buildAssistantTimelineEntry({
          id: "entry-assistant-actions",
          messageId: "message-assistant-actions",
          text: "Done",
          turnId: TURN_ID,
        }),
      ],
      latestTurn: {
        turnId: TURN_ID,
        state: "completed",
        startedAt: MESSAGE_CREATED_AT,
        completedAt: LATER_CREATED_AT,
      },
      turnDiffSummaryByAssistantMessageId: new Map([
        [
          assistantMessageId,
          {
            turnId: TURN_ID,
            checkpointTurnCount: 1,
            checkpointRef: CheckpointRef.make("checkpoint-actions"),
            status: "ready" as const,
            files: [{ path: "src/a.ts", kind: "modified", additions: 0, deletions: 0 }],
            assistantMessageId,
            completedAt: LATER_CREATED_AT,
          },
        ],
      ]),
      onOpenTurnDiff,
    });

    const fold = Array.from(container.querySelectorAll<HTMLButtonElement>("button")).find(
      (button) =>
        button.getAttribute("aria-expanded") === "false" && button.textContent?.includes("Worked"),
    );
    expect(fold).toBeDefined();
    expect(container.textContent).not.toContain("display command");
    await click(fold!);
    expect(fold?.getAttribute("aria-expanded")).toBe("true");
    expect(container.textContent).toContain("display command");

    const viewDiff = Array.from(container.querySelectorAll<HTMLButtonElement>("button")).find(
      (button) => button.textContent === "View diff",
    );
    expect(viewDiff).toBeDefined();
    await click(viewDiff!);
    expect(onOpenTurnDiff).toHaveBeenCalledWith(TURN_ID, "src/a.ts");
  });

  it("toggles a work row with Enter and Space while preventing their defaults", async () => {
    const container = await mountTimeline({
      activeTurnInProgress: true,
      timelineEntries: [
        buildWorkTimelineEntry("work-keyboard", {
          label: "Inspect",
          itemType: "mcp_tool_call",
          toolData: { value: 1 },
          rawCommand: "inspect --raw",
          changedFiles: ["src/changed.ts"],
        }),
      ],
    });
    const workRow = container.querySelector<HTMLElement>(
      'div[role="button"][aria-label^="Inspect"]',
    );
    expect(workRow).not.toBeNull();
    expect(workRow?.querySelector("pre")).toBeNull();

    const enter = new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true });
    await act(async () => workRow!.dispatchEvent(enter));
    expect(enter.defaultPrevented).toBe(true);
    expect(workRow?.querySelector("pre")?.textContent).toContain('"value": 1');

    const space = new KeyboardEvent("keydown", { key: " ", bubbles: true, cancelable: true });
    await act(async () => workRow!.dispatchEvent(space));
    expect(space.defaultPrevented).toBe(true);
    expect(workRow?.querySelector("pre")).toBeNull();

    const escape = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    await act(async () => workRow!.dispatchEvent(escape));
    expect(escape.defaultPrevented).toBe(false);
    expect(workRow?.querySelector("pre")).toBeNull();
  });

  it("preserves the work-group anchor while toggling hidden entries", async () => {
    const scrollToOffset = vi.fn();
    const listRef = {
      current: {
        getState: () => ({ scroll: 50 }),
        scrollToOffset,
      },
    } as unknown as React.RefObject<LegendListRef | null>;
    const container = await mountTimeline({
      listRef,
      timelineEntries: [
        buildWorkTimelineEntry("work-1", { label: "One", command: "echo one" }),
        buildWorkTimelineEntry("work-2", { label: "Two", command: "echo two" }),
        buildWorkTimelineEntry("work-3", { label: "Three", command: "echo three" }),
      ],
    });
    const toggle = Array.from(container.querySelectorAll<HTMLButtonElement>("button")).find(
      (button) =>
        button.getAttribute("aria-expanded") === "false" &&
        button.textContent?.includes("previous tool call"),
    );
    expect(toggle).toBeDefined();
    const anchor = toggle?.closest<HTMLElement>("[data-timeline-row-id]");
    expect(anchor).not.toBeNull();
    vi.spyOn(anchor!, "getBoundingClientRect")
      .mockReturnValueOnce({ bottom: 100 } as DOMRect)
      .mockReturnValueOnce({ bottom: 115 } as DOMRect);

    await click(toggle!);

    expect(scrollToOffset).toHaveBeenCalledWith({ offset: 65, animated: false });
    expect(toggle?.getAttribute("aria-expanded")).toBe("true");
    expect(toggle?.textContent).toContain("Show fewer");
  });
});

it("renders the queue through the shared user body renderer exactly once after working", async () => {
  const MessagesTimeline = await loadMessagesTimeline();
  const message = {
    id: MessageId.make("queue-card"),
    role: "user" as const,
    text: "Queued body",
    turnId: null,
    createdAt: "2026-09-22T12:00:00Z",
    updatedAt: "2026-09-22T12:00:00Z",
    streaming: false,
    delivery: { state: "queued" as const, provider: ProviderDriverKind.make("codex") },
  };
  const markup = renderToStaticMarkup(
    <MessagesTimeline
      {...buildProps()}
      isWorking
      timelineEntries={[]}
      queuedMessages={[message]}
      queuedStatuses={[
        {
          label: "Waiting for you",
          canSteer: false,
          steerDisabledReason: "Waiting for you",
          canCancel: true,
          steering: false,
          primaryAction: "send-now",
          canSendNow: true,
          sendNowDisabledReason: null,
        },
      ]}
    />,
  );
  expect(markup).toContain('data-queued-message-row="queue-card"');
  expect(markup).toContain('data-user-message-body="true"');
  expect(markup.match(/Queued body/g)).toHaveLength(1);
  expect(markup.indexOf('data-timeline-row-kind="working"')).toBeLessThan(
    markup.indexOf('data-timeline-row-kind="queued-message"'),
  );
});
