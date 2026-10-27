import { ProviderDriverKind } from "@coddy/contracts";
// @vitest-environment happy-dom
import { act, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { MessageId, type ChatAttachmentId } from "@coddy/contracts";
import { TooltipProvider } from "../ui/tooltip";
import { QueuedMessageTimelineRow } from "./QueuedMessageTimelineRow";

type Props = ComponentProps<typeof QueuedMessageTimelineRow>;
let container: HTMLDivElement;
let root: Root;
const onSteer = vi.fn();
const onCancel = vi.fn();
const onSendNow = vi.fn();
const props: Props = {
  message: {
    id: MessageId.make("queued"),
    role: "user",
    text: "Keep my work",
    turnId: null,
    createdAt: "2026-09-22T12:00:00Z",
    updatedAt: "2026-09-22T12:00:00Z",
    streaming: false,
    delivery: {
      state: "queued",
      provider: ProviderDriverKind.make("codex"),
      detail: "The turn ended before steering. Try Send now.",
    },
    attachments: [
      {
        type: "file",
        id: "f" as ChatAttachmentId,
        name: "test.txt",
        mimeType: "text/plain",
        sizeBytes: 1,
      },
    ],
  },
  isHead: true,
  status: {
    label: "Sends when the turn ends. Steer to send it now.",
    canSteer: true,
    steerDisabledReason: null,
    canCancel: true,
    steering: false,
    primaryAction: "steer",
    canSendNow: false,
    sendNowDisabledReason: "Wait for the running turn to end",
  },
  onSteer,
  onCancel,
  onSendNow,
  resolving: false,
  error: null,
  timestampFormat: "locale",
  children: <p>Keep my work</p>,
};
async function render(overrides: Partial<Props> = {}) {
  await act(async () =>
    root.render(
      <TooltipProvider delay={0}>
        <QueuedMessageTimelineRow {...props} {...overrides} />
      </TooltipProvider>,
    ),
  );
}
function button(label: string) {
  const node = container.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`);
  expect(node).not.toBeNull();
  return node!;
}
beforeEach(() => {
  vi.clearAllMocks();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

describe("QueuedMessageTimelineRow", () => {
  it("renders the body, attachment count, queue status and rejection detail", async () => {
    await render();
    expect(container.textContent).toContain("Keep my work");
    expect(container.textContent).toContain("1 attachment");
    expect(container.querySelector('[role="status"]')?.textContent).toContain("Queued");
    expect(container.textContent).toContain(props.status.label);
    expect(container.textContent).toContain("The turn ended before steering");
    expect(container.querySelector('[data-queued-message-row="queued"]')).not.toBeNull();
  });
  it("steers and cancels by id while preserving composer focus on pointer-down", async () => {
    await render();
    const steer = button("Steer into the running turn");
    const cancel = button("Cancel and return to the composer");
    for (const target of [steer, cancel]) {
      const event = new PointerEvent("pointerdown", { bubbles: true, cancelable: true });
      target.dispatchEvent(event);
      expect(event.defaultPrevented).toBe(true);
      await act(async () => target.click());
    }
    expect(onSteer).toHaveBeenCalledWith(props.message.id);
    expect(onCancel).toHaveBeenCalledWith(props.message.id);
  });
  it("explains disabled steer through a tooltip and accessible description", async () => {
    const reason = "Respond to the pending approval first";
    await render({ status: { ...props.status, canSteer: false, steerDisabledReason: reason } });
    const steer = button("Steer into the running turn");
    expect(steer.disabled).toBe(true);
    expect(steer.getAttribute("aria-description")).toBe(reason);
    const trigger = steer.closest('[data-slot="tooltip-trigger"]')!;
    await act(async () => {
      trigger.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    });
    expect(document.body.textContent).toContain(reason);
  });
  it("shows only the queue-order label on non-head cards", async () => {
    await render({
      isHead: false,
      status: {
        ...props.status,
        label: "Sends after the messages above.",
        primaryAction: null,
        canSteer: false,
        steerDisabledReason: "Send the messages above first",
      },
    });
    expect(container.textContent).toContain("Sends after the messages above.");
    expect(container.textContent).not.toContain("Send the messages above first");
    expect(container.querySelector("[data-queued-message-steer]")).toBeNull();
    expect(button("Cancel and return to the composer").disabled).toBe(false);
  });

  it("hides unsupported steer and displays the reason", async () => {
    await render({
      status: {
        ...props.status,
        primaryAction: null,
        canSteer: false,
        steerDisabledReason: "This provider cannot steer a running turn",
      },
    });
    expect(container.querySelector("[data-queued-message-steer]")).toBeNull();
    expect(container.textContent).toContain("This provider cannot steer a running turn");
  });
  it("uses Send now for a held head", async () => {
    await render({
      status: {
        ...props.status,
        label: "Waiting for you",
        primaryAction: "send-now",
        canSteer: false,
        canSendNow: true,
        sendNowDisabledReason: null,
      },
    });
    await act(async () => button("Send now").click());
    expect(onSendNow).toHaveBeenCalledWith(props.message.id);
  });
  it("disables both buttons during Steering and shows local errors in place", async () => {
    await render({
      status: {
        ...props.status,
        label: "Steering…",
        steering: true,
        canSteer: false,
        canCancel: false,
        steerDisabledReason: "Steering…",
      },
      error: "Could not cancel. Try again.",
    });
    expect(button("Steer into the running turn").disabled).toBe(true);
    expect(button("Cancel and return to the composer").disabled).toBe(true);
    expect(container.textContent).toContain("Steering…");
    expect(container.querySelector('[role="alert"]')?.textContent).toBe(
      "Could not cancel. Try again.",
    );
  });
});
