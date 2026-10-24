import { describe, expect, it } from "vite-plus/test";

import {
  queuedCardIdsInVisualOrder,
  type DesktopUiQueuedCardObservation,
} from "./queued-card-order.ts";

const second: DesktopUiQueuedCardObservation = {
  id: "second-id",
  text: "second Queued Sends when the turn ends.",
  y: 200,
  displayed: true,
};
const third: DesktopUiQueuedCardObservation = {
  id: "third-id",
  text: "third Queued Sends after the messages above.",
  y: 300,
  displayed: true,
};

describe("packaged queue visual FIFO observation", () => {
  it("follows screen position while the virtualizer DOM slots are reversed", () => {
    expect(queuedCardIdsInVisualOrder([third, second], ["second", "third"])).toEqual([
      "second-id",
      "third-id",
    ]);
  });

  it("rejects visually reversed FIFO even when DOM order matches the prompts", () => {
    expect(
      queuedCardIdsInVisualOrder(
        [
          { ...second, y: 300 },
          { ...third, y: 200 },
        ],
        ["second", "third"],
      ),
    ).toBeNull();
  });

  it("waits for distinct visible positions and durable identities", () => {
    for (const changed of [
      { ...third, displayed: false },
      { ...third, id: second.id },
      { ...third, id: "" },
      { ...third, y: second.y },
      { ...third, y: Number.NaN },
      { ...third, y: Number.POSITIVE_INFINITY },
    ]) {
      expect(queuedCardIdsInVisualOrder([second, changed], ["second", "third"])).toBeNull();
    }
  });

  it("ignores a leftover node the virtualizer keeps off screen", () => {
    const leftover = { ...third, displayed: false };
    expect(queuedCardIdsInVisualOrder([leftover], [])).toEqual([]);
    expect(queuedCardIdsInVisualOrder([second, leftover], ["second"])).toEqual(["second-id"]);
  });

  it("does not accept partial queues or stale card text", () => {
    expect(queuedCardIdsInVisualOrder([second], ["second", "third"])).toBeNull();
    expect(
      queuedCardIdsInVisualOrder(
        [second, { ...third, text: "fourth Queued" }],
        ["second", "third"],
      ),
    ).toBeNull();
    expect(queuedCardIdsInVisualOrder([], [])).toEqual([]);
  });
});
