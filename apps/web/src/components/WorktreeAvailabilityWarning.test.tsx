import { type VcsAdoptedWorktreeStatus } from "@coddy/contracts";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vite-plus/test";

import { WorktreeAvailabilityWarning } from "./WorktreeAvailabilityWarning";

const missing: VcsAdoptedWorktreeStatus = {
  threadId: "thread-one",
  worktreeKey: "worktree-one",
  path: "/repo/worktrees/feature-one",
  branch: "feature/one",
  availability: "missing-registered",
  registrationState: "prunable",
  locked: false,
} as VcsAdoptedWorktreeStatus;

describe("WorktreeAvailabilityWarning", () => {
  it("renders durable recovery detail and accessible actions for a missing registration", () => {
    const markup = renderToStaticMarkup(
      <WorktreeAvailabilityWarning status={missing} onRetry={vi.fn()} onRemove={vi.fn()} />,
    );

    expect(markup).toContain('role="alert"');
    expect(markup).toContain("feature/one");
    expect(markup).toContain("/repo/worktrees/feature-one");
    expect(markup).toContain("Git registration remains");
    expect(markup).toContain("Git registration:");
    expect(markup).toContain("Prunable (stale)");
    expect(markup).toContain("Retry detection");
    expect(markup).toContain("Remove from Coddy Studio");
  });

  it("shows lock and missing-unregistered state without inventing cleanup", () => {
    const locked = renderToStaticMarkup(
      <WorktreeAvailabilityWarning
        status={{ ...missing, locked: true, lockReason: "Kept by another tool" }}
        onRetry={vi.fn()}
        onRemove={vi.fn()}
      />,
    );
    const unregistered = renderToStaticMarkup(
      <WorktreeAvailabilityWarning
        status={{
          ...missing,
          availability: "missing-unregistered",
          registrationState: null,
        }}
        onRetry={vi.fn()}
        onRemove={vi.fn()}
      />,
    );

    expect(locked).toContain("Kept by another tool");
    expect(unregistered).toContain("Git no longer registers this worktree");
    expect(unregistered).toContain("Not registered");
    expect(unregistered).not.toContain("Clean stale");
  });

  it("shows the concrete registered and unknown states", () => {
    const registered = renderToStaticMarkup(
      <WorktreeAvailabilityWarning
        status={{ ...missing, registrationState: "registered" }}
        onRetry={vi.fn()}
        onRemove={vi.fn()}
      />,
    );
    const unknown = renderToStaticMarkup(
      <WorktreeAvailabilityWarning
        status={{
          ...missing,
          availability: "verification-unavailable",
          registrationState: null,
        }}
        onRetry={vi.fn()}
        onRemove={vi.fn()}
      />,
    );

    expect(registered).toContain("Registered");
    expect(registered).not.toContain("Prunable (stale)");
    expect(unknown).toContain("Unknown");
  });

  it("renders verification and removing states but nothing for a present workspace", () => {
    expect(
      renderToStaticMarkup(
        <WorktreeAvailabilityWarning
          status={{ ...missing, availability: "verification-unavailable" }}
          onRetry={vi.fn()}
          onRemove={vi.fn()}
        />,
      ),
    ).toContain("could not be verified");
    expect(
      renderToStaticMarkup(
        <WorktreeAvailabilityWarning
          status={{ ...missing, availability: "removing" }}
          onRetry={vi.fn()}
          onRemove={vi.fn()}
        />,
      ),
    ).toContain("Removal is in progress");
    expect(
      renderToStaticMarkup(
        <WorktreeAvailabilityWarning
          status={{ ...missing, availability: "present" }}
          onRetry={vi.fn()}
          onRemove={vi.fn()}
        />,
      ),
    ).toBe("");
  });
});
