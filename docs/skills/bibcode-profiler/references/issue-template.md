# Issue and comment templates

Fill every `<…>` from the diagnosis, and delete any section that has nothing
true to say. Write repo-relative paths (`apps/server/src/…:42`) and short
commit SHAs (12 characters); the privacy gate rejects home paths and 40-character
hex runs. Replace project and worktree names in paths with placeholders such as
`~/<worktrees>/<name>`: they are the user's data.

## New issue

Title: `<kind>: <the finding's title, generalized>`, under 80 characters. For
example: `rpc.slow: orchestration.dispatchCommand takes over 10 s on the server`.

Labels: `bug` and `profiler:<fingerprint>`.

Body:

```markdown
<!-- profiler-fingerprint: <fingerprint> -->

## Summary

<One paragraph: what the user experienced and what the profiler recorded.>

## Environment

| Field       | Value                                                                       |
| ----------- | --------------------------------------------------------------------------- |
| BiBCode     | <version>, <desktop (AppImage, DMG or NSIS) or server (package or archive)> |
| OS          | <OS name and version, architecture>                                         |
| Mode        | <local, or Remote Server over SSH, relay or LAN>                            |
| Finding     | `<id>`: <severity>, seen <count> times from <firstSeen> to <lastSeen>       |
| Fingerprint | `<fingerprint>`                                                             |
| Capture     | native dumps <nativeDumps>, OS crash records <osCrashRecords>               |

## Evidence

- `<sid>:<seq>` `<event kind>`: <one-line description>
- <Crash sidecar `<crash-id>`: the minidump stays on the reporter's machine.>

## Stack

<Symbolicated frames, top first; omit for non-crash findings.>

    #0 <function> (<repo-relative file>:<line>)
    #1 …

## Suspected source

- `<repo-relative path>:<line>`: <why this code is implicated>

## Reproduction

1. <step>
2. <step>

## Root-cause hypothesis

<Hypothesis.> Confidence: <high, medium or low>, because <reason>.

## Suggested fix direction

<Direction, not a patch.>
```

## Comment on a duplicate

Post this only when the new evidence extends the issue: a new version, OS or
architecture, a higher count since the last report, a new or newly symbolicated
stack, or a new slow part.

```markdown
New occurrence of `<id>` (profiler fingerprint `<fingerprint>`):

| Field   | Value                                                  |
| ------- | ------------------------------------------------------ |
| BiBCode | <version> (the issue last reported <previous version>) |
| OS      | <OS name and version, architecture>                    |
| Count   | <count>, last seen <lastSeen>                          |

What is new: <the new version, OS, count, stack or slow part, in one or two lines>.

<New evidence, stack or suspected source, in the issue's format.>
```
