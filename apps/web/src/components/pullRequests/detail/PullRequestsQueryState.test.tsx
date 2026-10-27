// @vitest-environment happy-dom
import { PullRequestsOperationError } from "@coddy/contracts";
import * as Cause from "effect/Cause";
import { AsyncResult } from "effect/reactivity";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { EnvironmentQueryView } from "../../../state/query";
import { mount } from "../review/testHelpers";
import { PullRequestsQueryState } from "./PullRequestsQueryState";

function queryView(
  overrides: Partial<EnvironmentQueryView<unknown>> = {},
): EnvironmentQueryView<unknown> {
  return {
    data: null,
    error: null,
    isPending: false,
    emission: AsyncResult.initial(),
    refresh: vi.fn(),
    revalidate: vi.fn(),
    requiresRetry: false,
    ...overrides,
  };
}

let view: Awaited<ReturnType<typeof mount>>;

beforeEach(() => {
  vi.clearAllMocks();
});

afterEach(async () => {
  await view?.unmount();
});

describe("PullRequestsQueryState", () => {
  it("renders the alert banner and children when data and an error are both present", async () => {
    const error = new PullRequestsOperationError({
      operation: "get",
      code: "provider_error",
      message: "GitLab timed out",
      hostDetail: "Try again in a moment.",
      retryable: true,
    });
    view = await mount(
      <PullRequestsQueryState
        label="merge request"
        query={queryView({
          data: { title: "Fix timeout" },
          error: error.message,
          emission: AsyncResult.failure(Cause.fail(error)),
        })}
      >
        <p>Merge request body</p>
      </PullRequestsQueryState>,
    );

    expect(document.querySelector('[role="alert"]')?.textContent).toContain("GitLab timed out");
    expect(document.querySelector('[role="alert"]')?.textContent).toContain(
      "Try again in a moment.",
    );
    expect(view.container.textContent).toContain("Merge request body");
  });

  it("renders only the alert when there is an error and no data", async () => {
    const error = new PullRequestsOperationError({
      operation: "get",
      code: "provider_error",
      message: "GitLab timed out",
      hostDetail: null,
      retryable: true,
    });
    view = await mount(
      <PullRequestsQueryState
        label="merge request"
        query={queryView({
          data: null,
          error: error.message,
          emission: AsyncResult.failure(Cause.fail(error)),
        })}
      >
        <p>Merge request body</p>
      </PullRequestsQueryState>,
    );

    expect(document.querySelector('[role="alert"]')?.textContent).toContain("GitLab timed out");
    expect(view.container.textContent).not.toContain("Merge request body");
  });
});
