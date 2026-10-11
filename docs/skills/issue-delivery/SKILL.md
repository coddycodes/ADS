---
name: issue-delivery
description: Keep multi-issue repository work focused on verified acceptance and closure. Use for ongoing issue batches or repeated failed validation runs.
---

# Issue delivery

Use the issue's acceptance criteria as the delivery checklist. Keep the original
scope and the user's deferred issues. A commit or a unit-test count is supporting
evidence, not an accepted screenshot, native result, or closed issue.

- Choose one blocking acceptance criterion per issue. Give each delegate one
  concrete deliverable, an output path, a stopping condition, and the user's
  model setting. Stop overlapping implementation and duplicate reviews.
- Confirm the delegate is running before waiting. Use `followup_task` to start a
  new turn on a completed delegate; `send_message` only delivers a message and
  does not wake it. A queued instruction is not evidence of live review work.
- Keep analysis short. Choose the next evidence-producing action within about
  30 words. Do not repeat background reasoning between tool calls.
- Test the complete caller before a costly run, including preparation, invocation,
  cleanup, and comparison of the final inputs. A component test does not prove
  the wrapper. Preserve the distinction between compatibility and native evidence.
- Before another CI dispatch, write a private delivery checkpoint and run
  `node scripts/check-agent-delivery.mjs <checkpoint.json>` from its candidate
  checkout. Refresh run status from GitHub before filling `runs`. Join live runs.
  A changed commit alone does not justify repeating a failed recipe.
- After two rounds without accepted evidence, stop adding diagnostics. Identify
  the disproved premise and the smallest next action that addresses it. Record
  that revision in `replan`. Do not weaken requirements to manufacture progress.
- Run focused checks after a meaningful change. Run required broad checks once
  on the reviewed candidate. Repeat only for changed code, a failure, or a
  concrete unresolved concern. Keep large logs private and summarize the result.
- Report closed issues, accepted requirements, the remaining blocker, and the
  next delivery checkpoint. State uncertainty in estimates. Never present a
  timeout limit as an expected completion time.

The checkpoint has this shape. Replace the source with the actual commit and
the example data with fresh evidence. Keep execution data outside the repository.

```json
{
  "requirement": "The issue acceptance criterion this run will prove",
  "source": "0000000000000000000000000000000000000000",
  "proof": {
    "source": "0000000000000000000000000000000000000000",
    "scope": "complete-caller",
    "exitCode": 0
  },
  "next": {
    "scenario": "selected-scenario",
    "changedBehavior": "The reproduced defect corrected by this recipe"
  },
  "runs": [],
  "rounds": [],
  "replan": {
    "premise": "The assumption disproved by the last runs",
    "evidence": "The actual source or runtime result and the next test"
  }
}
```

`rounds` records whether each round produced accepted evidence. `runs` contains
the scenario and current status: `queued`, `running`, `failed`, or `complete`.
`replan` is required after two unsuccessful rounds. Link the actual proof in the
private task record. The checker validates the decision record; it does not
replace review of that proof or verify a native runtime itself.
