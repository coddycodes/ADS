---
name: bibcode-release
description: Use when releasing BiBCode, cutting a version, tagging a release, or publishing a release.
---

# BiBCode release

Read [the release runbook](../../operations/release.md) in full before
acting. It owns the commands, branch flow, recovery rules, and publication checks.

- Confirm the version, channel, candidate commit, and authorized release actions.
- Follow **Local Verification** for the complete workspace test graph and
  release checks; record failures and unavailable checks.
- Follow **Maintainer branch flow** for version preparation, tree comparisons,
  the local merge, and the annotated tag on `main`.
- Use **Failed release runs** to distinguish infrastructure retries from test
  hardening and to check the conditions for replacing an unpublished tag.
- Follow **Stable Release Runbook** and **Draft inspection** for assets,
  signatures, curated notes, human inspection, and the approval dispatch.
- Verify publication, the REST latest endpoint, finalization, and `develop`
  synchronization. Report the exact revision, commands, results, and remaining
  native validation.
