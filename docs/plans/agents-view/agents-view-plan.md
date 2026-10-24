# Agents View Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps
> use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A cross-environment "Agents" section in the left panel between the Search row
and the Projects header, with server-pushed conversation previews on the thread-shell
stream.

**Architecture:** One additive wire field (`conversationPreview`) on
`OrchestrationThreadShell`, populated in the Rust shell builder from projection data the
server already loads; all aggregation, grouping, filtering, and unread policy is
client-side over the existing `threadShellsAtom`. No new subscriptions or RPC methods.

**Tech Stack:** Rust (Axum/Tokio server), TypeScript (React 19, Effect Schema, Effect
Atom, zustand, TanStack Router), Vitest via `vp test`.

**Spec:** `docs/plans/agents-view/agents-view-spec.md` — decisions D1–D15 and pinned
contracts §3 are normative; this plan implements them and nothing more. Research:
`research-orca-agents-view.md` (§8 = the reference surface), `research-bibcode-left-panel.md`.

## Global Constraints

- The spec must be approved before implementation starts (AGENTS.md design gate).
- Zero reference-product ("Orca") strings in code, identifiers, UI copy, or comments.
- `packages/contracts` stays schema-only; the wire change is additive
  (`Schema.optional`), and the RPC Rust-parity manifest fingerprint is regenerated in
  the same task that changes the schema.
- Preview caps are normative: prompt 200 chars, tool 160, assistantMessage 320,
  `char`-boundary truncation with `…` appended only when truncated (spec §3.2).
- Status policy single source: groups derive from `resolveThreadStatusPill` (spec D13).
- No list virtualization; volume bounds per spec D10. No activity-actor subscriptions.
- User's standing workflow preferences: implementation may be delegated via
  `codex:rescue` with the coordinating agent reviewing every diff; all `apps/web`
  changes are verified against the `vercel-react-best-practices` skill before the task
  is considered done.
- Every task: focused tests first (TDD), then `vp check` and `vp run typecheck` at the
  end of the task if TypeScript changed; Rust tasks additionally
  `cargo fmt --all --check`, `cargo test -p bibcode-server <filter>`, and
  `cargo clippy -p bibcode-server --all-targets -- -D warnings`.
- Same-patch documentation obligations are Task 7; the feature is not complete without
  them (spec §6).
- Commit after each task with a conventional message; do not stage `.codegraph/`.

---

### Task 1: Contracts — `OrchestrationConversationPreview`

**Files:**

- Modify: `packages/contracts/src/orchestration.ts` (insert before
  `OrchestrationThreadShell` at ~:497; add field to the shell struct after
  `unresolvedDelivery` at ~:532)
- Test: `packages/contracts/src/orchestration.test.ts`
- Regenerate: the manifest consumed by `packages/contracts/src/rpcRustParity.test.ts`
  (schema fingerprints)

**Interfaces:**

- Produces: `OrchestrationConversationPreview` (schema + type) and
  `OrchestrationThreadShell.conversationPreview?: OrchestrationConversationPreview | null | undefined`,
  imported by Tasks 3 and 5 from `@bibcode/contracts`.

- [ ] **Step 1: Write the failing test** — append to
      `packages/contracts/src/orchestration.test.ts`, matching the file's existing
      `describe`/`it` + `Schema.decodeUnknownSync` style:

```ts
describe("OrchestrationConversationPreview", () => {
  const decodeShell = Schema.decodeUnknownSync(OrchestrationThreadShell);

  it("decodes a shell without the field (older server)", () => {
    const shell = decodeShell(baseShellFixture);
    expect(shell.conversationPreview).toBeUndefined();
  });

  it("decodes a populated preview", () => {
    const shell = decodeShell({
      ...baseShellFixture,
      conversationPreview: {
        prompt: "fix the flaky test",
        tool: "Bash: vp test apps/web",
        assistantMessage: "The test is flaky because…",
      },
    });
    expect(shell.conversationPreview).toEqual({
      prompt: "fix the flaky test",
      tool: "Bash: vp test apps/web",
      assistantMessage: "The test is flaky because…",
    });
  });

  it("decodes null members and a null preview", () => {
    expect(
      decodeShell({ ...baseShellFixture, conversationPreview: null }).conversationPreview,
    ).toBeNull();
    expect(
      decodeShell({
        ...baseShellFixture,
        conversationPreview: { prompt: null, tool: null, assistantMessage: null },
      }).conversationPreview,
    ).toEqual({ prompt: null, tool: null, assistantMessage: null });
  });
});
```

Reuse the file's existing minimal valid shell fixture if one exists; otherwise define
`baseShellFixture` locally with the required `OrchestrationThreadShell` members
(`id`, `projectId`, `title`, `modelSelection`, `runtimeMode`, `branch`,
`worktreePath`, `latestTurn`, `createdAt`, `updatedAt`, `session`,
`latestUserMessageAt`, `hasPendingApprovals`, `hasPendingUserInput`,
`hasActionableProposedPlan`) copied from an existing fixture in that test file.

- [ ] **Step 2: Run the test to verify it fails**

Run: `vp test packages/contracts/src/orchestration.test.ts`
Expected: FAIL — `OrchestrationConversationPreview` / `conversationPreview` unknown.

- [ ] **Step 3: Implement the schema** in `packages/contracts/src/orchestration.ts`,
      immediately above `OrchestrationThreadShell`:

```ts
export const OrchestrationConversationPreview = Schema.Struct({
  /** Newest user message text, truncated server-side to ≤ 200 chars. */
  prompt: Schema.NullOr(TrimmedNonEmptyString),
  /** Newest tool activity summary of the running latest turn, ≤ 160 chars. */
  tool: Schema.NullOr(TrimmedNonEmptyString),
  /** Newest assistant message text, truncated server-side to ≤ 320 chars. */
  assistantMessage: Schema.NullOr(TrimmedNonEmptyString),
});
export type OrchestrationConversationPreview = typeof OrchestrationConversationPreview.Type;
```

and inside `OrchestrationThreadShell` after `unresolvedDelivery`:

```ts
conversationPreview: Schema.optional(Schema.NullOr(OrchestrationConversationPreview)),
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `vp test packages/contracts/src/orchestration.test.ts`
Expected: PASS.

- [ ] **Step 5: Regenerate the parity fingerprint**

Run: `vp test packages/contracts/src/rpcRustParity.test.ts`
Expected: FAIL with the manifest's own regeneration instructions (fingerprint drift for
the shell schema). Follow those instructions exactly (the failing assertion names the
manifest file and the expected update), then re-run until PASS. Do not hand-edit
anything the instructions don't name.

- [ ] **Step 6: Typecheck and commit**

Run: `vp run typecheck && vp check`
Expected: PASS.

```bash
git add packages/contracts/src/orchestration.ts packages/contracts/src/orchestration.test.ts <manifest file from step 5>
git commit -m "feat(contracts): add conversationPreview to the thread shell"
```

---

### Task 2: Server — populate `conversationPreview` in the shell builder

**Files:**

- Modify: `apps/server/src/production/orchestration_rpc.rs` — `thread_shell` (:1011),
  its two call sites (`shell_snapshot` :869, thread detail :899), and the inline
  `mod tests` (:1153)

**Interfaces:**

- Consumes: `Snapshot { messages: Vec<ProjectionThreadMessage>, activities:
  Vec<ProjectionThreadActivity>, turns: Vec<ProjectionTurn>, .. }`
  (`apps/server/src/orchestration/engine.rs:6302`), `ProjectionThreadMessage { thread_id,
  role, text, created_at, .. }`, `ProjectionThreadActivity { thread_id, turn_id, tone,
  summary, created_at, .. }` (`apps/server/src/persistence/repositories.rs:1849-1874`).
- Produces: JSON key `conversationPreview` on every thread object in the
  `orchestration.subscribeShell` snapshot — shape exactly per Task 1's schema. The
  thread-detail path passes no preview and emits no key (wire change is shell-only,
  spec §3.1).

- [ ] **Step 1: Write the failing tests** in the existing `mod tests` of
      `orchestration_rpc.rs`, following its fixture style (plain synchronous `#[test]` fns
      are fine — the functions under test are pure):

```rust
#[test]
fn conversation_preview_truncates_on_char_boundary() {
    let text = "é".repeat(300);
    let truncated = truncate_preview(&text, 200);
    assert_eq!(truncated.chars().count(), 201); // 200 chars + '…'
    assert!(truncated.ends_with('…'));
    assert_eq!(truncate_preview("short", 200), "short");
}

#[test]
fn conversation_preview_picks_newest_rows_and_gates_tool_on_running_turn() {
    // Build a Snapshot fixture with one thread, two user messages, two assistant
    // messages, a tool activity on the latest turn, and a latest turn in state
    // "running" (reuse/extend the module's existing snapshot fixture helper if one
    // exists; otherwise construct the vectors directly).
    let snapshot = preview_snapshot_fixture("running");
    let previews = build_conversation_previews(&snapshot);
    let preview = previews.get("thread-1").expect("preview for thread-1");
    assert_eq!(preview.prompt.as_deref(), Some("newest user prompt"));
    assert_eq!(preview.assistant_message.as_deref(), Some("newest assistant text"));
    assert_eq!(preview.tool.as_deref(), Some("Edit: src/main.rs"));

    // A completed latest turn suppresses the tool line.
    let done = preview_snapshot_fixture("completed");
    let previews = build_conversation_previews(&done);
    assert_eq!(previews.get("thread-1").expect("preview").tool, None);
}

#[test]
fn thread_shell_embeds_preview_and_detail_omits_it() {
    let snapshot = preview_snapshot_fixture("running");
    let previews = build_conversation_previews(&snapshot);
    let thread = &snapshot.threads[0];
    let with = thread_shell(thread, &snapshot, previews.get(thread.thread_id.as_str()));
    assert_eq!(
        with["conversationPreview"]["prompt"],
        serde_json::json!("newest user prompt")
    );
    let without = thread_shell(thread, &snapshot, None);
    assert!(without.get("conversationPreview").is_none());
}
```

`preview_snapshot_fixture(latest_turn_state: &str)` is a test helper you write in the
same module: one `ProjectionThread` (`thread_id: "thread-1"`, `latest_turn_id:
Some("turn-2")`), messages `[user "older prompt", assistant "older assistant", user
"newest user prompt", assistant "newest assistant text"]` with ascending `created_at`,
one `ProjectionThreadActivity { turn_id: Some("turn-2"), tone: "tool", summary:
"Edit: src/main.rs", .. }`, and one `ProjectionTurn` for `turn-2` whose `state` is the
argument. Copy timestamp/fixture conventions from the module's existing tests.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p bibcode-server conversation_preview`
Expected: FAIL — `truncate_preview` / `build_conversation_previews` not found.

- [ ] **Step 3: Implement.** In `orchestration_rpc.rs`, above `thread_shell`:

```rust
const PREVIEW_PROMPT_MAX_CHARS: usize = 200;
const PREVIEW_TOOL_MAX_CHARS: usize = 160;
const PREVIEW_ASSISTANT_MAX_CHARS: usize = 320;

#[derive(Debug, Default)]
struct ConversationPreview {
    prompt: Option<String>,
    tool: Option<String>,
    assistant_message: Option<String>,
}

fn truncate_preview(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

/// One pass over messages and activities so shell building stays
/// O(messages + activities + threads) even though every engine event
/// re-sends the full shell snapshot.
fn build_conversation_previews(
    snapshot: &crate::orchestration::Snapshot,
) -> HashMap<&str, ConversationPreview> {
    let mut previews: HashMap<&str, ConversationPreview> = HashMap::new();
    let mut newest_user: HashMap<&str, &crate::persistence::ProjectionThreadMessage> =
        HashMap::new();
    let mut newest_assistant: HashMap<&str, &crate::persistence::ProjectionThreadMessage> =
        HashMap::new();
    for message in &snapshot.messages {
        let bucket = match message.role.as_str() {
            "user" => &mut newest_user,
            "assistant" => &mut newest_assistant,
            _ => continue,
        };
        match bucket.get(message.thread_id.as_str()) {
            Some(existing) if existing.created_at >= message.created_at => {}
            _ => {
                bucket.insert(message.thread_id.as_str(), message);
            }
        }
    }
    let running_latest_turn: HashMap<&str, &str> = snapshot
        .threads
        .iter()
        .filter_map(|thread| {
            let latest_id = thread.latest_turn_id.as_deref()?;
            let running = snapshot.turns.iter().any(|turn| {
                turn.thread_id == thread.thread_id
                    && turn.turn_id.as_deref() == Some(latest_id)
                    && turn.state == "running"
            });
            running.then_some((thread.thread_id.as_str(), latest_id))
        })
        .collect();
    let mut newest_tool: HashMap<&str, &crate::persistence::ProjectionThreadActivity> =
        HashMap::new();
    for activity in &snapshot.activities {
        if activity.tone != "tool" {
            continue;
        }
        let Some(latest_id) = running_latest_turn.get(activity.thread_id.as_str()) else {
            continue;
        };
        if activity.turn_id.as_deref() != Some(latest_id) {
            continue;
        }
        match newest_tool.get(activity.thread_id.as_str()) {
            Some(existing) if existing.created_at >= activity.created_at => {}
            _ => {
                newest_tool.insert(activity.thread_id.as_str(), activity);
            }
        }
    }
    for (thread_id, message) in newest_user {
        previews.entry(thread_id).or_default().prompt =
            Some(truncate_preview(&message.text, PREVIEW_PROMPT_MAX_CHARS));
    }
    for (thread_id, message) in newest_assistant {
        previews.entry(thread_id).or_default().assistant_message =
            Some(truncate_preview(&message.text, PREVIEW_ASSISTANT_MAX_CHARS));
    }
    for (thread_id, activity) in newest_tool {
        previews.entry(thread_id).or_default().tool =
            Some(truncate_preview(&activity.summary, PREVIEW_TOOL_MAX_CHARS));
    }
    previews
}
```

(Adjust the `crate::persistence::…` paths to the module's actual imports — the file
already imports these projection types.)

**Trimming is mandatory, not hygiene**: the client decodes each member as
`TrimmedNonEmptyString`, whose non-empty check runs **after** decode-time trimming
(`packages/contracts/src/baseSchemas.ts:5-14`) — a whitespace-only preview string
would fail the member decode and with it the whole shell snapshot for that
environment. Therefore, in the three population loops, use
`message.text.trim()` / `activity.summary.trim()`: skip the row when the trimmed
text is empty, and truncate the trimmed text, never the raw one. Add to the Step 1
fixture one user message whose text is `"  padded prompt \n"` (newest) and assert the
emitted preview is `"padded prompt"`; add one whitespace-only assistant message
(newest) and assert `assistant_message` falls back to `None` rather than emitting
whitespace.

Change `thread_shell` to accept the preview and emit the key only when given one:

```rust
fn thread_shell(
    thread: &ProjectionThread,
    snapshot: &crate::orchestration::Snapshot,
    preview: Option<&ConversationPreview>,
) -> Value {
    // …existing body unchanged…
    let mut shell = json!({ /* existing fields exactly as today */ });
    if let Some(preview) = preview {
        shell["conversationPreview"] = json!({
            "prompt": preview.prompt,
            "tool": preview.tool,
            "assistantMessage": preview.assistant_message,
        });
    }
    shell
}
```

(`build_conversation_previews` only creates an entry when at least one member is
populated, so a thread with no preview content gets no map entry and the key is
simply omitted — the same wire shape an older server produces.)

Call sites: in `shell_snapshot` (:865-870) build the index once and pass per-thread —

```rust
let previews = build_conversation_previews(&snapshot);
let threads = snapshot
    .threads
    .iter()
    .filter(|thread| thread.deleted_at.is_none() && (thread.archived_at.is_some()) == archived)
    .map(|thread| thread_shell(thread, &snapshot, previews.get(thread.thread_id.as_str())))
    .collect::<Vec<_>>();
```

In the thread-detail builder (:899) pass `None`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p bibcode-server conversation_preview && cargo test -p bibcode-server thread_shell`
Expected: PASS (including pre-existing shell tests, updated only if they assert exact
key sets).

- [ ] **Step 5: Rust gate and commit**

Run: `cargo fmt --all --check && cargo clippy -p bibcode-server --all-targets -- -D warnings`
Expected: clean.

```bash
git add apps/server/src/production/orchestration_rpc.rs
git commit -m "feat(server): push conversation previews on the shell stream"
```

---

### Task 3: Client policy — `agentsSection.logic.ts`

**Files:**

- Create: `apps/web/src/components/sidebar/agentsSection.logic.ts`
- Test: `apps/web/src/components/sidebar/agentsSection.logic.test.ts`

**Interfaces:**

- Consumes: `EnvironmentThreadShell` (`@bibcode/client-runtime` — shell +
  `environmentId`), `resolveThreadStatusPill` and `ThreadStatusPill` from
  `../Sidebar.logic.ts`, `normalizeSearchText` from `../CommandPalette.logic.ts`,
  `scopedThreadKey` from `@bibcode/client-runtime/environment/scoped` (verify the
  exact import specifier at the top of `Sidebar.tsx`), `EnvironmentShellAvailability`
  from `@bibcode/client-runtime/state/shell`.
- Produces (Tasks 5 and 6 import these exact names):

```ts
export type AgentGroupId = "working" | "blocked" | "waiting" | "done";
export const AGENT_GROUP_ORDER: ReadonlyArray<{ id: AgentGroupId; label: string }>;
export const AGENTS_GROUP_PREVIEW_COUNT = 5;
export const AGENTS_FILTER_MAX_BYTES = 2048;
export interface AgentRow {
  readonly key: string; // scopedThreadKey
  readonly ref: ScopedThreadRef;
  readonly shell: EnvironmentThreadShell;
  readonly group: AgentGroupId;
  readonly pill: ThreadStatusPill | null;
  readonly environmentLabel: string;
  readonly environmentLive: boolean;
  readonly environmentStatus: EnvironmentAvailabilityStatus | null;
  readonly projectTitle: string;
  readonly previewLine: string | null;
  readonly searchText: string;
}
export interface AgentGroup {
  readonly id: AgentGroupId;
  readonly label: string;
  readonly rows: ReadonlyArray<AgentRow>;
}
export function resolveAgentGroup(pill: ThreadStatusPill | null): AgentGroupId;
export function resolveAgentPreviewLine(
  pill: ThreadStatusPill | null,
  preview: OrchestrationConversationPreview | null | undefined,
): string | null;
export function buildAgentRows(input: {
  readonly shells: ReadonlyArray<EnvironmentThreadShell>;
  readonly projectTitleById: ReadonlyMap<string, string>;
  readonly environmentLabelById: ReadonlyMap<string, string>;
  readonly availabilityByEnvironmentId: ReadonlyMap<string, EnvironmentAvailabilityStatus>;
}): ReadonlyArray<AgentRow>;
export function groupAgentRows(
  rows: ReadonlyArray<AgentRow>,
  query: string,
): ReadonlyArray<AgentGroup>;
```

- [ ] **Step 1: Write the failing tests** in `agentsSection.logic.test.ts` (Vitest,
      same import style as `environmentRail.logic.test.ts`). Cover, with hand-built shell
      fixtures (helper `makeShell(overrides)` returning a minimal
      `EnvironmentThreadShell`):

```ts
describe("resolveAgentGroup", () => {
  it("maps pill labels to groups per spec §3.3", () => {
    expect(resolveAgentGroup({ label: "Working", ...pillRest })).toBe("working");
    expect(resolveAgentGroup({ label: "Connecting", ...pillRest })).toBe("working");
    expect(resolveAgentGroup({ label: "Pending Approval", ...pillRest })).toBe("blocked");
    expect(resolveAgentGroup({ label: "Awaiting Input", ...pillRest })).toBe("waiting");
    expect(resolveAgentGroup({ label: "Plan Ready", ...pillRest })).toBe("waiting");
    expect(resolveAgentGroup({ label: "Completed", ...pillRest })).toBe("done");
    expect(resolveAgentGroup(null)).toBe("done");
  });
});

describe("resolveAgentPreviewLine", () => {
  it("shows the tool line only while working, else assistant, else prompt", () => {
    const preview = { prompt: "p", tool: "Bash: ls", assistantMessage: "a" };
    expect(resolveAgentPreviewLine(workingPill, preview)).toBe("Bash: ls");
    expect(resolveAgentPreviewLine(completedPill, preview)).toBe("a");
    expect(resolveAgentPreviewLine(completedPill, { ...preview, assistantMessage: null })).toBe(
      "p",
    );
    expect(resolveAgentPreviewLine(completedPill, null)).toBeNull();
    expect(resolveAgentPreviewLine(completedPill, undefined)).toBeNull();
  });
});

describe("buildAgentRows", () => {
  it("includes only non-archived shells with a session", () => {
    /* archived + sessionless excluded */
  });
  it("marks rows stale when availability is not 'live'", () => {
    /* environmentLive false, status carried */
  });
  it("builds a lowercase haystack containing title, project, branch, env label, provider, pill label, previews", () => {});
});

describe("groupAgentRows", () => {
  it("orders groups working → blocked → waiting → done and elides empty groups", () => {});
  it("sorts rows by updatedAt desc with key tie-break", () => {});
  it("filters by normalized substring and fails closed past 2048 bytes", () => {
    expect(groupAgentRows(rows, "x".repeat(3000))).toEqual([]);
  });
});
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `vp test apps/web/src/components/sidebar/agentsSection.logic.test.ts`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement** `agentsSection.logic.ts`:

```ts
export const AGENT_GROUP_ORDER = [
  { id: "working", label: "Working" },
  { id: "blocked", label: "Pending Approval" },
  { id: "waiting", label: "Awaiting Input" },
  { id: "done", label: "Done" },
] as const satisfies ReadonlyArray<{ id: AgentGroupId; label: string }>;

export function resolveAgentGroup(pill: ThreadStatusPill | null): AgentGroupId {
  switch (pill?.label) {
    case "Working":
    case "Connecting":
      return "working";
    case "Pending Approval":
      return "blocked";
    case "Awaiting Input":
    case "Plan Ready":
      return "waiting";
    default:
      return "done";
  }
}

export function resolveAgentPreviewLine(
  pill: ThreadStatusPill | null,
  preview: OrchestrationConversationPreview | null | undefined,
): string | null {
  if (preview === null || preview === undefined) return null;
  if ((pill?.label === "Working" || pill?.label === "Connecting") && preview.tool !== null) {
    return preview.tool;
  }
  return preview.assistantMessage ?? preview.prompt ?? null;
}
```

`buildAgentRows` filters `shell.archivedAt === null && shell.session !== null`, calls
`resolveThreadStatusPill({ thread: shell })` once per row, derives `group`,
`previewLine`, `environmentLive = availability === "live"`, and precomputes
`searchText = normalizeSearchText([title, projectTitle, branch ?? "", environmentLabel,
shell.session?.providerName ?? "", pill?.label ?? "", preview strings].join(" "))`.
`groupAgentRows` returns `[]` when `new TextEncoder().encode(query).length >
AGENTS_FILTER_MAX_BYTES`; otherwise filters by
`row.searchText.includes(normalizeSearchText(query))` (empty query keeps all), buckets
by `group`, sorts each bucket by `updatedAt` desc then `key` ordinal, and maps
`AGENT_GROUP_ORDER`, dropping empty groups.

- [ ] **Step 4: Run tests to verify they pass**

Run: `vp test apps/web/src/components/sidebar/agentsSection.logic.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/web/src/components/sidebar/agentsSection.logic.ts apps/web/src/components/sidebar/agentsSection.logic.test.ts
git commit -m "feat(web): agents section row/group/filter policy"
```

---

### Task 4: Persisted expansion state in `uiStateStore`

**Files:**

- Modify: `apps/web/src/uiStateStore.ts` (add keys beside `projectExpandedById`, pure
  helpers beside `resolveProjectExpanded`/`setProjectExpanded` :331-362)
- Test: `apps/web/src/uiStateStore.test.ts` (append)

**Interfaces:**

- Produces (Task 5 imports): `UiState.agentsSectionExpanded: boolean` (default `true`),
  `UiState.agentsGroupExpandedById: Record<string, boolean>`, and pure updaters
  `setAgentsSectionExpanded(state: UiState, expanded: boolean): UiState`,
  `setAgentsGroupExpanded(state: UiState, groupId: string, expanded: boolean): UiState`,
  plus resolver
  `resolveAgentsGroupExpanded(agentsGroupExpandedById: Readonly<Record<string, boolean>>, groupId: string): boolean`
  returning the stored value, else `groupId !== "done"` (spec: DONE collapsed by
  default).

- [ ] **Step 1: Write the failing tests** (follow the file's existing test style for
      `setProjectExpanded`):

```ts
describe("agents section expansion", () => {
  it("defaults the section expanded and the done group collapsed", () => {
    expect(initialUiState.agentsSectionExpanded).toBe(true);
    expect(resolveAgentsGroupExpanded({}, "done")).toBe(false);
    expect(resolveAgentsGroupExpanded({}, "working")).toBe(true);
    expect(resolveAgentsGroupExpanded({ done: true }, "done")).toBe(true);
  });

  it("updates immutably and no-ops on same value", () => {
    const collapsed = setAgentsSectionExpanded(initialUiState, false);
    expect(collapsed.agentsSectionExpanded).toBe(false);
    expect(setAgentsSectionExpanded(collapsed, false)).toBe(collapsed);
    const next = setAgentsGroupExpanded(initialUiState, "done", true);
    expect(next.agentsGroupExpandedById).toEqual({ done: true });
    expect(setAgentsGroupExpanded(next, "done", true)).toBe(next);
  });
});
```

(Use the file's actual exported initial-state name; if persisted-state migration is
sanitized on load, extend that sanitizer for the two new keys the same way the existing
keys are handled.)

- [ ] **Step 2: Run tests to verify they fail** —
      `vp test apps/web/src/uiStateStore.test.ts` → FAIL.

- [ ] **Step 3: Implement** the two state keys (defaults `true` / `{}`), the two
      no-op-preserving updaters (mirror `setProjectExpanded`'s early-return shape), and the
      resolver.

- [ ] **Step 4: Run tests to verify they pass** — same command, PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/web/src/uiStateStore.ts apps/web/src/uiStateStore.test.ts
git commit -m "feat(web): persist agents section and group expansion"
```

---

### Task 5: The `AgentsSection` component

**Files:**

- Create: `apps/web/src/components/sidebar/AgentsSection.tsx`
- Test: `apps/web/src/components/sidebar/AgentsSection.test.tsx`
- Modify: `apps/web/src/components/Sidebar.tsx` (mount inside
  `SidebarProjectsContent`'s return, between the search-row `SidebarGroup` closing at
  :3864 and the ARM64 warning block at :3865; pass `navigateToThread` down — it already
  reaches `SidebarProjectsContent` callers, follow how `handleNewThread` is plumbed)

**Interfaces:**

- Consumes: `useThreadShells`, `useProjects`, `setActiveEnvironmentId`
  (`apps/web/src/state/entities.ts`), `useEnvironments`
  (`apps/web/src/state/environments.ts`), `useEnvironmentShellSummary`
  (`apps/web/src/state/shell.ts`), Task 3's policy module, Task 4's expansion state,
  `useSidebarWorkspaceMetaStore`/`selectIsUnread`/`markRead`
  (`apps/web/src/sidebarWorkspaceMetaStore.ts`), sidebar primitives
  (`SidebarGroup`, `SidebarMenu`, `SidebarMenuItem`, `SidebarMenuButton` from
  `../ui/sidebar`).
- Produces: `export const AgentsSection: React.MemoExoticComponent<...>` with props
  `{ navigateToThread: (ref: ScopedThreadRef) => void }`.

Component contract (all policy comes from Task 3 — the component only renders):

- Header row: chevron + uppercase `Agents` label + total-row-count pill; clicking
  toggles `agentsSectionExpanded`. `data-testid="agents-section-header"`.
- When expanded: filter `<input placeholder="Filter agents…"
  data-testid="agents-filter-input">` (local `useState`, passed through
  `useDeferredValue` before `groupAgentRows`), then groups.
- Group header: label + count pill, toggles `agentsGroupExpandedById[group.id]`,
  `data-testid={"agents-group-" + group.id}`.
- Rows: `SidebarMenuButton` per row; title `font-semibold` while
  `selectIsUnread(unreadThreadKeys, row.key)`; pill dot/label with the pill's
  `colorClass`/`dotClass`/`pulse`; `projectTitle · branch` line; `previewLine`
  (single `truncate` line); environment label badge; when `!row.environmentLive`,
  wrapper gets `opacity-60` and the badge shows `row.environmentStatus`. Relative time
  from `row.shell.updatedAt` (reuse the sidebar's existing relative-time helper — find
  it via the thread rows' timestamp rendering in `Sidebar.tsx`).
- Row click: `markRead(row.key)`, `setActiveEnvironmentId(row.ref.environmentId)`,
  `navigateToThread(row.ref)`.
- Per-group cap: slice to `AGENTS_GROUP_PREVIEW_COUNT` unless the group is in a local
  `expandedOverflow` set; "Show more (N)" / "Show less" toggle row.
- Empty row set: header + muted "No agents yet" line.
- Accessibility (spec §3.4): each group's row container renders `role="list"`; every
  row button carries an `aria-label` of the form
  `` `${title}, ${pill?.label ?? "Done"}, ${environmentLabel}` ``.
- Memoize: component wrapped in `memo`; row list built in one `useMemo` over
  `[shells, projects, environments, availability]`; a dedicated `AgentsRow` memo
  component so one shell update re-renders one row.

- [ ] **Step 1: Write the failing component tests** (Testing Library, follow
      `EnvironmentRail.test.tsx` for render/mocking conventions — mock the state hooks the
      same way that file mocks its atoms). Cover: (1) groups render in pinned order with
      counts and DONE collapsed by default; (2) filter text narrows rows and >2048-byte
      query renders no rows; (3) row click calls `markRead`, `setActiveEnvironmentId`, and
      `navigateToThread` with the row's ref; (4) stale environment row is greyed and shows
      the availability status; (5) section header toggle collapses the body; (6) unread key
      bolds the title; (7) empty state renders.

- [ ] **Step 2: Run tests to verify they fail** —
      `vp test apps/web/src/components/sidebar/AgentsSection.test.tsx` → FAIL.

- [ ] **Step 3: Implement the component**, then mount it in
      `SidebarProjectsContent`:

```tsx
</SidebarGroup>
<AgentsSection navigateToThread={navigateToThread} />
{showArm64IntelBuildWarning && arm64IntelBuildWarningDescription ? (
```

(Add `navigateToThread` to `SidebarProjectsContentProps` and pass it from the
`Sidebar()` call site at ~:4756 where the existing `navigateToThread` callback
(:2273) is in scope.)

- [ ] **Step 4: Run tests to verify they pass** — component tests plus
      `vp test apps/web/src/components/Sidebar.test.tsx` (existing sidebar tests must stay
      green; update snapshots/queries only where the new section legitimately appears).

- [ ] **Step 5: React quality gate** — verify the new/changed `apps/web` code against
      the `vercel-react-best-practices` skill; fix findings.

- [ ] **Step 6: Commit**

```bash
git add apps/web/src/components/sidebar/AgentsSection.tsx apps/web/src/components/sidebar/AgentsSection.test.tsx apps/web/src/components/Sidebar.tsx
git commit -m "feat(web): agents section in the left panel"
```

---

### Task 6: Unread rising-edge trigger

**Files:**

- Create: `apps/web/src/components/sidebar/useAgentsUnread.ts`
- Test: `apps/web/src/components/sidebar/useAgentsUnread.test.ts`
- Modify: `apps/web/src/components/sidebar/AgentsSection.tsx` (mount the hook)

**Interfaces:**

- Consumes: `EnvironmentThreadShell.latestTurn` (`turnId`, `state`), the open route's
  thread (read via TanStack Router `useParams` on `/$environmentId/$threadId` —
  copy how `Sidebar.tsx` derives `routeThreadKey`), `markUnread` from
  `useSidebarWorkspaceMetaStore`.
- Produces: `export function useAgentsUnread(rows: ReadonlyArray<AgentRow>): void`,
  plus the exported pure transition detector Task 6 tests directly:

```ts
export function detectUnreadTransitions(input: {
  readonly previous: ReadonlyMap<string, string>; // key → `${turnId}:${state}`
  readonly rows: ReadonlyArray<AgentRow>;
  readonly openThreadKey: string | null;
}): { readonly next: ReadonlyMap<string, string>; readonly markUnreadKeys: ReadonlyArray<string> };
```

- [ ] **Step 1: Write the failing tests** for `detectUnreadTransitions`:

```ts
it("marks unread when the latest turn transitions into a settled state", () => {
  const previous = new Map([["k1", "turn-1:running"]]);
  const { markUnreadKeys } = detectUnreadTransitions({
    previous,
    rows: [rowWithTurn("k1", "turn-1", "completed")],
    openThreadKey: null,
  });
  expect(markUnreadKeys).toEqual(["k1"]);
});

it("does not mark the open route thread, unchanged states, or first observations", () => {
  // open thread: openThreadKey === "k1" → no mark
  // unchanged: previous already `turn-1:completed` → no mark
  // first observation of an already-settled turn (fresh map) → tracked, not marked
});

it("treats interrupted and error like completed, and running/null as not-settled", () => {});
```

- [ ] **Step 2: Run tests to verify they fail** — module not found.

- [ ] **Step 3: Implement.** `detectUnreadTransitions`: settled =
      `state === "completed" || state === "interrupted" || state === "error"`; for each row
      with a `latestTurn`, signature `` `${turnId}:${state}` ``; mark when the previous map
      **has** the key, the signature changed, the new state is settled, and
      `row.key !== openThreadKey`; always record the new signature. `useAgentsUnread`
      keeps the map in a `useRef`, runs the detector in `useEffect` on `rows`, and calls
      `markUnread` for each returned key.

- [ ] **Step 4: Run tests to verify they pass**, then mount `useAgentsUnread(rows)`
      inside `AgentsSection` and re-run the Task 5 component tests.

  **Which rows (pinned):** the hook receives the **full `buildAgentRows` output** —
  before `groupAgentRows`, before filtering, before the per-group cap — and both the
  rows `useMemo` and the hook call sit unconditionally at the top of `AgentsSection`,
  running even while `agentsSectionExpanded` is `false`. Wiring it to the
  filtered/capped/displayed rows would silently miss transitions while a filter is
  typed, a group is collapsed, or a row sits past the show-more cap.

Run: `vp test apps/web/src/components/sidebar/`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/web/src/components/sidebar/useAgentsUnread.ts apps/web/src/components/sidebar/useAgentsUnread.test.ts apps/web/src/components/sidebar/AgentsSection.tsx
git commit -m "feat(web): bold agents rows until visited"
```

---

### Task 7: Documentation amendments and final gate

**Files:**

- Modify: `docs/plans/remote-servers/remote-servers-spec.md` (§4.8 — add the
  Agents-section exception sentence pinned in the spec §3.6)
- Modify: `docs/architecture/connection-runtime.md` (presentation-scoping paragraph —
  same one-sentence exception)
- Modify: `docs/architecture/rpc-and-orchestration.md` (document
  `conversationPreview` on the shell contract: additive, caps, population semantics)
- Review: `docs/testing/` runbooks (packaged-UI-flow rule)

- [ ] **Step 1: Amend the three living documents** with exactly the scope above — the
      exception sentence: _"Exception: the Agents section in the left panel is the single
      cross-environment surface; it ignores rail selection by design, and clicking one of
      its rows re-points rail selection to the row's environment so every other surface
      remains scoped."_

- [ ] **Step 2: Review `docs/testing/` runbooks.** If a native visual-validation
      runbook enumerates sidebar sections or packaged UI flows, add the Agents section to
      that enumeration; otherwise record in the final report that the runbooks were
      **reviewed and remain accurate**.

- [ ] **Step 3: Full validation gate**

Run, in order, and record outputs:

```bash
vp check
vp run typecheck
vp test packages/contracts apps/web/src/components/sidebar apps/web/src/uiStateStore.test.ts apps/web/src/components/Sidebar.test.tsx
cargo fmt --all --check
cargo test -p bibcode-server
cargo clippy -p bibcode-server --all-targets -- -D warnings
git diff && git status --short   # no unintended edits, no .codegraph/, no debug output
```

- [ ] **Step 4: Commit docs and report**

```bash
git add docs/plans/remote-servers/remote-servers-spec.md docs/architecture/connection-runtime.md docs/architecture/rpc-and-orchestration.md docs/testing
git commit -m "docs: record the agents-view shell field and rail-scoping exception"
```

Final report must list the exact validation commands run, anything that could not run,
and residual risk (spec §6).

---

## Self-review notes (kept for executors)

- Spec §3.1–§3.2 → Tasks 1–2; §3.3 → Task 3; §3.4 UI → Tasks 5–6; §3.5 → Task 4;
  §3.6/§6 docs → Task 7. D6's group labels and D10's caps appear verbatim in Tasks 3–5.
- Names used across tasks: `OrchestrationConversationPreview` /
  `conversationPreview` (Tasks 1, 2, 3), `build_conversation_previews` /
  `truncate_preview` / `ConversationPreview` (Task 2), `AgentGroupId` / `AgentRow` /
  `AgentGroup` / `AGENT_GROUP_ORDER` / `AGENTS_GROUP_PREVIEW_COUNT` /
  `AGENTS_FILTER_MAX_BYTES` / `resolveAgentGroup` / `resolveAgentPreviewLine` /
  `buildAgentRows` / `groupAgentRows` (Tasks 3, 5, 6), `setAgentsSectionExpanded` /
  `setAgentsGroupExpanded` / `resolveAgentsGroupExpanded` (Tasks 4, 5),
  `useAgentsUnread` / `detectUnreadTransitions` (Task 6), `AgentsSection` (Tasks 5–7).
- Line numbers cited (`Sidebar.tsx:3864`, `orchestration_rpc.rs:1011` etc.) are
  anchors verified on 2026-08-31 against `mubeda/develop-3`; re-locate by symbol name
  if the file has drifted.

---

## V2 tasks — full Agents view (spec §7, confirmed 2026-09-01)

Execute in order; each follows the same TDD steps, review checklist, and
per-task gates as Tasks 1–7. Verified anchors: `ChatView` props
`{ environmentId, threadId, routeKind: "server" }`
(`apps/web/src/routes/_chat.$environmentId.$threadId.tsx:69-73`);
`AppSidebarLayout` (`apps/web/src/components/AppSidebarLayout.tsx:55-104`)
currently renders the sidebar unconditionally; route files live in
`apps/web/src/routes/` (`createFileRoute` file-route convention).

### Task 8: Logic — group-by modes, unread filter, view groups

**Files:** modify `apps/web/src/components/sidebar/agentsSection.logic.ts`;
test `agentsSection.logic.test.ts`.
**Produces:** `AgentsGroupByMode = "status" | "project" | "environment"`;
`interface AgentViewGroup { id: string; label: string; rows: ReadonlyArray<AgentRow> }`;
`buildAgentViewGroups(rows, options: { query: string; groupBy: AgentsGroupByMode; unreadOnly: boolean; unreadThreadKeys: ReadonlyArray<string>; selectedKey: string | null }): ReadonlyArray<AgentViewGroup>`;
`countUnreadAgentRows(rows, unreadThreadKeys): number`.
Semantics (spec D18/D21/D22): filter query exactly as `groupAgentRows`
(byte-cap fails closed); `unreadOnly` keeps rows that are unread **or** match
`selectedKey`; `status` mode = fixed v1 order with `status:`-prefixed ids;
`project` / `environment` modes bucket by `projectTitle` / `environmentLabel`
(fallback label "Unknown"), groups ordered by newest member `updatedAt`, ids
`project:<title>` / `environment:<label>`; rows recency-sorted in every mode.
TDD: cover each mode, the unread-only + selected-row exception, and the
empty-group elision. Keep `groupAgentRows` untouched (deleted in Task 10).

### Task 9: The `/agents` route and page

**Files:** create `apps/web/src/routes/agents.tsx`,
`apps/web/src/components/agents/AgentsPage.tsx`,
`apps/web/src/components/agents/AgentsPage.test.tsx`; modify
`apps/web/src/components/AppSidebarLayout.tsx` (pathname check via
`useLocation`: on `/agents` render `children` inside `SidebarProvider`
without `Sidebar`/`EnvironmentRail`/`SidebarControl`); modify
`apps/web/src/uiStateStore.ts` only if the mode-scoped collapse key helper
needs adjusting (`resolveAgentsGroupExpanded(map, `"${mode}:${groupId}"`)` —
only `status:done` defaults collapsed).
**Page contract (spec §7.2):** top strip: back button (`aria-label "Back"`,
`router.history.back()`, fallback `navigate({ to: "/" })` when history is
empty), title "agents", `"{n} unread"` badge from `countUnreadAgentRows`.
Left column (fixed width ~340px, `data-testid="agents-view-list"`): toolbar
(filter input reused semantics, group-by `Select` default `status`,
unread-only bell `Toggle`, kebab `DropdownMenu` with "Mark all read" calling
`markRead` for every unread row key), then collapsible groups from
`buildAgentViewGroups`, rows reusing the v1 row anatomy (extract the row
renderer from `AgentsSection.tsx` into
`apps/web/src/components/agents/AgentsRow.tsx` so Task 10 can delete the
section without losing it). Row click: select (page state) + `markRead`;
hover "jump to workspace" button: `markRead` + `setActiveEnvironmentId` +
`navigateToThread`. Detail pane: selected row → `<ChatView
environmentId=… threadId=… routeKind="server" />`; none → centered "Select an
agent to view its activity"; selection cleared when its key leaves the row
set. Tests: takeover layout (no sidebar), back navigation, selection mounts
ChatView (mock it), unread badge, mark-all-read, group-by switch.

### Task 10: Sidebar swap — nav row replaces the section

**Files:** create `apps/web/src/components/sidebar/AgentsNavRow.tsx` (+ test);
modify `apps/web/src/components/Sidebar.tsx` (mount nav row where
`<AgentsSection …>` sat; drop the `navigateToThread` prop plumbing if now
unused); delete `AgentsSection.tsx` + `AgentsSection.test.tsx`; move
`useAgentsUnread(rows)` into `AgentsNavRow` (full `buildAgentRows` output,
unconditional — spec §7.2 trigger placement); delete `groupAgentRows` and its
tests from the logic module (Task 8's `buildAgentViewGroups` replaces it).
Nav row: `SidebarMenuButton` with label "Agents", unread-count badge
(`countUnreadAgentRows`), `data-testid="agents-nav-row"`, click →
`navigate({ to: "/agents" })`, `aria-current` when the route is active.
Tests: badge count, navigation, unread trigger still marks while on normal
routes; `Sidebar.test.tsx` updated for the removed section.

### Task 11: Docs + full battery + live verification

Update `docs/user/workspace-ui.md` and `docs/reference/encyclopedia.md`
(section wording → nav row + full view per spec §7.3), the exception
sentences in `docs/architecture/connection-runtime.md` and
`docs/plans/remote-servers/remote-servers-spec.md` §4.8 (cross-environment
surface = the Agents view and its nav badge), and
`docs/testing/cross-platform-validation.md`. Then the supervisor runs the
full battery (Tasks 7 gate) and a Playwright pass matching the reference
screenshots: nav row → view opens (list + "Select an agent…"), row select →
embedded live session, back arrow → normal view.
