import { describe, expect, it } from "vite-plus/test";
import * as Cause from "effect/Cause";
import * as Socket from "effect/socket/Socket";

import { classifyDisconnect } from "./livenessProtocol.ts";

const closed = (code: number) =>
  Cause.fail(new Socket.SocketError({ reason: new Socket.SocketCloseError({ code }) }));

describe("classifyDisconnect", () => {
  it("reports our own liveness close with the silent time", () => {
    expect(classifyDisconnect(closed(4408), 30_000)).toEqual({
      _tag: "LivenessTimeout",
      idleMs: 30_000,
    });
  });

  it("reports a close frame from the server with its code", () => {
    expect(classifyDisconnect(closed(1000), null)).toEqual({ _tag: "Closed", code: 1000 });
    expect(classifyDisconnect(closed(1012), null)).toEqual({ _tag: "Closed", code: 1012 });
  });

  it("keeps a 4408 close frame sent by the server as a closed connection (plan ruling 3)", () => {
    // Only the client's own liveness close is a liveness timeout; a close
    // frame the server sent truthfully means the server closed the connection.
    expect(classifyDisconnect(closed(4408), null)).toEqual({ _tag: "Closed", code: 4408 });
  });

  it("reports an abnormal closure or a socket error as a lost connection", () => {
    expect(classifyDisconnect(closed(1006), null)).toEqual({ _tag: "Lost" });
    expect(
      classifyDisconnect(
        Cause.fail(
          new Socket.SocketError({ reason: new Socket.SocketReadError({ cause: "reset" }) }),
        ),
        null,
      ),
    ).toEqual({ _tag: "Lost" });
    expect(classifyDisconnect(Cause.interrupt(), null)).toEqual({ _tag: "Lost" });
  });
});
