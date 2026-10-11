import {
  ActivityRecordId,
  ActivityScopeId,
  ProviderDriverKind,
  ThreadId,
  type ActivityActorSummary,
  type ActivityActorControl,
  type ActivitySnapshot,
} from "@bibcode/contracts";
import {
  ActivityRoster,
  type ActivityRosterProps,
} from "../../src/components/activity/ActivityRoster";
import type { Fixture } from "./types";

const noChange = () => undefined;
const startedAt = "2026-10-10T12:00:00.000Z";
const now = "2026-10-10T12:02:00.000Z";
const scopeId = ActivityScopeId.make("typography-activity-scope");
function actor(
  id: string,
  name: string,
  patch: Partial<ActivityActorSummary> = {},
): ActivityActorSummary {
  return {
    _tag: "actor",
    id: ActivityRecordId.make(id),
    name,
    status: "running",
    summary: "Inspect the real roster row text.",
    startedAt,
    updatedAt: now,
    terminalAt: null,
    parentActorId: null,
    role: "reviewer",
    providerType: "codex",
    ...patch,
  };
}
const parent = actor("typography-parent", "Typography coordinator");
const child = actor("typography-child", "Typography child", {
  parentActorId: parent.id,
  role: "implementer",
});
const unavailable = actor("typography-unavailable", "Unavailable stop actor");
const completed = actor("typography-completed", "Completed typography reviewer", {
  status: "completed",
  terminalAt: now,
});
const controls: readonly ActivityActorControl[] = [
  { actorId: parent.id, state: "available", controlRevision: 1, activeDescendantCount: 1 },
  { actorId: child.id, state: "requested", controlRevision: 1, activeDescendantCount: 0 },
];
const snapshot: ActivitySnapshot = {
  protocolVersion: 2,
  scopeId,
  scope: { _tag: "thread", threadId: ThreadId.make("typography-activity-thread") },
  revision: 1,
  provider: ProviderDriverKind.make("codex"),
  providerInstanceId: null,
  capabilities: {
    actors: true,
    attributedActivity: true,
    backgroundWork: false,
    historyRecovery: "full",
    terminalObservation: false,
    targetedActorCancellation: true,
  },
  observationState: "live",
  sections: {
    subagents: { state: "live", message: null, retryable: false },
    backgroundTasks: { state: "unsupported", message: null, retryable: false },
  },
  counts: { subagents: { active: 3, done: 1 }, backgroundTasks: { active: 0, done: 0 } },
  actors: [parent, child, unavailable, completed],
  workItems: [],
  actorsHasMore: false,
  workItemsHasMore: false,
  control: { scopeId, revision: 1, actors: controls, operations: [] },
  updatedAt: now,
};
const props: ActivityRosterProps = {
  section: "subagents",
  snapshot,
  active: {
    pages: [{ records: [parent, child, unavailable], actorControls: controls, nextCursor: null }],
    loading: false,
    error: null,
  },
  done: {
    pages: [{ records: [completed], actorControls: [], nextCursor: null }],
    loading: false,
    error: null,
  },
  reconciled: { active: [parent, child, unavailable], done: [completed] },
  now,
  notification: null,
  focusRecordId: null,
  onFocusRestored: noChange,
  onSelect: noChange,
  onLoadMore: noChange,
  onCancelActor: noChange,
};
export const activityContextFixtures: readonly Fixture[] = [
  {
    id: "activity-roster",
    title: "Activity roster status, summary and elapsed time",
    modules: ["components/activity/ActivityRoster.tsx"],
    owners: ["ActivityRoster", "ActivityRecordRow"],
    anchors: [
      "Typography coordinator",
      "Running",
      "Inspect the real roster row text.",
      "reviewer",
      "Child agent",
      "Elapsed 2m",
      "Completed in 2m",
      "Stop unavailable",
      "Stopping",
    ],
    render: () => <ActivityRoster {...props} />,
  },
];
