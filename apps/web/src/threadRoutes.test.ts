import { describe, expect, it } from "vite-plus/test";
import { scopeThreadRef } from "@coddy/client-runtime/environment";
import { ThreadId } from "@coddy/contracts";
import { DraftId } from "./composerDraftStore";

import {
  buildDraftThreadRouteParams,
  buildThreadRouteParams,
  missingRouteThreadRedirectDelay,
  resolveProjectRouteRef,
  resolveThreadRouteRef,
  resolveThreadRouteTarget,
  shouldRedirectMissingRouteThread,
} from "./threadRoutes";

describe("threadRoutes", () => {
  it("builds canonical thread route params from a scoped ref", () => {
    const ref = scopeThreadRef("env-1" as never, ThreadId.make("thread-1"));

    expect(buildThreadRouteParams(ref)).toEqual({
      environmentId: "env-1",
      threadId: "thread-1",
    });
  });

  it("resolves a scoped ref only when both params are present", () => {
    expect(
      resolveThreadRouteRef({
        environmentId: "env-1",
        threadId: "thread-1",
      }),
    ).toEqual({
      environmentId: "env-1",
      threadId: "thread-1",
    });

    expect(resolveThreadRouteRef({ environmentId: "env-1" })).toBeNull();
    expect(resolveThreadRouteRef({ threadId: "thread-1" })).toBeNull();
  });

  it("builds canonical draft route params from a draft id", () => {
    expect(buildDraftThreadRouteParams(DraftId.make("draft-1"))).toEqual({
      draftId: "draft-1",
    });
  });

  it("resolves draft and server route targets", () => {
    expect(
      resolveThreadRouteTarget({
        environmentId: "env-1",
        threadId: "thread-1",
      }),
    ).toEqual({
      kind: "server",
      threadRef: {
        environmentId: "env-1",
        threadId: "thread-1",
      },
    });

    expect(
      resolveThreadRouteTarget({
        draftId: "draft-1",
      }),
    ).toEqual({
      kind: "draft",
      draftId: "draft-1",
    });
  });
});

describe("shouldRedirectMissingRouteThread", () => {
  it("delays a live missing-thread redirect so newly created threads can materialize", () => {
    expect(
      missingRouteThreadRedirectDelay({
        shellStatus: "live",
        routeThreadExists: false,
        environmentHasServerThreads: true,
      }),
    ).toBe(1_000);
    expect(
      missingRouteThreadRedirectDelay({
        shellStatus: "live",
        routeThreadExists: true,
        environmentHasServerThreads: true,
      }),
    ).toBeNull();
  });

  it("redirects when a live snapshot lacks the thread but has other server threads", () => {
    expect(
      shouldRedirectMissingRouteThread({
        shellStatus: "live",
        routeThreadExists: false,
        environmentHasServerThreads: true,
      }),
    ).toBe(true);
  });

  it("never redirects while the shell is not live (backend flap/reconnect windows)", () => {
    for (const shellStatus of [
      "starting",
      "synchronizing",
      "degraded",
      "storage-changed",
      "recovery-required",
      "unavailable",
      "configuration-error",
      null,
    ] as const) {
      expect(
        shouldRedirectMissingRouteThread({
          shellStatus,
          routeThreadExists: false,
          environmentHasServerThreads: true,
        }),
      ).toBe(false);
    }
  });

  it("never redirects when the environment has no server threads (empty/partial early snapshot; drafts do not count)", () => {
    expect(
      shouldRedirectMissingRouteThread({
        shellStatus: "live",
        routeThreadExists: false,
        environmentHasServerThreads: false,
      }),
    ).toBe(false);
  });

  it("never redirects while the route thread exists", () => {
    expect(
      shouldRedirectMissingRouteThread({
        shellStatus: "live",
        routeThreadExists: true,
        environmentHasServerThreads: true,
      }),
    ).toBe(false);
  });
});

describe("resolveProjectRouteRef", () => {
  it("names the project of a project-scoped route and ignores thread routes", () => {
    expect(resolveProjectRouteRef({ environmentId: "env-a", projectId: "project-a" })).toEqual({
      environmentId: "env-a",
      projectId: "project-a",
    });
    expect(resolveProjectRouteRef({ environmentId: "env-a", threadId: "thread-a" })).toBeNull();
    expect(
      resolveProjectRouteRef({ environmentId: "env-a", projectId: "project-a", threadId: "t" }),
    ).toBeNull();
    expect(resolveProjectRouteRef({})).toBeNull();
  });
});
