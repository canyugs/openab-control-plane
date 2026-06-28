---
name: pr-review
description: How to review a pull request as a member of the OpenAB review council — as a reviewer or as the chair. Covers fetching the PR (read-only self-fetch), the in-thread protocol, the [done] close signal, the findings/verdict output format, and (chair only) posting one verdict comment via --body-file. Use whenever you are convened into a PR review session (a trigger naming owner/repo#N).
---

# PR Review Council — how to review

You are a bot in a multi-agent PR review council. The **session trigger** names *what*
to review this time — the PR (`owner/repo#N`) and your angle assignment, if any. This
skill is *how* to review; it is the standing behaviour, identical every session.

## Everyone

- Work in **THIS session thread only**. No other channels.
- End your final message with the token **`[done]`** on its own line — that is what
  records you as finished and closes the session. Send it exactly once, when truly
  done. (A bare 🆗 reaction also counts.) A 🆗 *in passing* mid-message is not a
  done-signal — only a trailing `[done]` or a standalone 🆗.
- If the trigger gave you an **angle assignment**, cover **only** the angle(s) on the
  row matching your bot name; ignore the rest.

## Reviewers — read-only, fetch it yourself

You have **read-only** GitHub access. The diff is **not** inlined in the trigger
(it may be large) — pull exactly what your angle needs:

- the diff: `gh pr diff N --repo owner/repo`
- large PR? start narrow: `gh pr diff N --repo owner/repo --name-only`, then read only the files your angle touches
- surrounding context: `gh pr checkout N` (or read files directly) — judge the change **in context, not in isolation**

Do **NOT** run `gh pr comment` / `gh pr review` — writes fail for you, and the chair
owns the PR comment. Post **one** message with all findings (no progress updates),
then `[done]`.

### Reviewer output format

```
## Findings

| # | Severity | Finding | Location |
|---|----------|---------|----------|
| 1 | 🔴      | <description> | `path/file.rs:42` |
| 2 | 🟡      | <description> | `path/file.rs:88` |
| 3 | 🟢      | <description> | — |

<details>
<summary>Finding Details</summary>

### 🔴 F1: <title>
<what's wrong, why it matters, where exactly>

**Fix:**
```suggestion
<concrete code or approach>
```

### 🟡 F2: <title>
...
</details>

Verdict: **approve** | **request changes**
```

- Every finding cites `path/file:line` and quotes the relevant code.
- Number findings (F1, F2, …) so the chair can reference them.
- Cover correctness, security, design, test coverage — or, if assigned an angle, that angle only.

### Severity

| Level | Meaning | Action |
|-------|---------|--------|
| 🔴 Critical | Correctness bug, security issue, data loss | Must fix before merge |
| 🟡 Minor | Style, naming, defense-in-depth, non-blocking | Should fix, not a blocker |
| 🟢 Info | Praise, context, future consideration | No action needed |

## Chair — the only writer

You are the only bot that may write to the PR. Maintain **exactly one** comment.

**Write the comment body to a FILE, then post with `--body-file`** — never pass
multi-line markdown via `--body "..."`, the shell mangles backticks, `$(...)`, and
newlines into garbage:

1. Write the full markdown verdict to `/tmp/verdict.md` (use your file-writing tool, not an inline `--body` string).
2. `gh pr comment N --repo owner/repo --edit-last --create-if-none --body-file /tmp/verdict.md`

`--edit-last --create-if-none` creates the comment the first time and **edits the SAME
comment** every run after, so the PR never accumulates duplicates. Post once as
in-progress, then overwrite `/tmp/verdict.md` and re-run for the synthesized verdict —
never run a plain `gh pr comment` (without `--edit-last`) a second time. Then `[done]`.

### Chair synthesis output format

```
<VERDICT LINE>

## What This PR Does
<one paragraph>

## How It Works
- <bullet points>

## Findings

| # | Severity | Finding | Location |
|---|----------|---------|----------|
| 1 | 🔴      | <description> (raised by: rev1, rev2) | `file:line` |
| 2 | 🟡      | <description> (raised by: rev1) | `file:line` |

<details>
<summary>Finding Details</summary>

### 🔴 F1: <title>
<merged description, cite who raised it>

**Fix:**
```suggestion
<concrete fix>
```
</details>

<details>
<summary>Unresolved Disagreements</summary>

### <topic>
- **rev1**: <position>
- **rev2**: <opposite position>
- **Chair call**: <which side and why>

(If none: "None — reviewers agree on all findings")
</details>

<details>
<summary>Review Coverage</summary>

- 🔴 Critical: N · 🟡 Minor: N · 🟢 Info: N
- Reviewers: rev1 (approve), rev2 (request changes)
- Consensus: **approve** | **request changes** | **split**
</details>
```

### Verdict line

First line of the synthesis, one of:

- `LGTM ✅` — no critical findings, approve
- `CHANGES REQUESTED ⚠️` — has 🔴 findings, request changes

### Optional chair PR actions

When the App identity / write scope allows it, after the synthesis comment:

- `gh pr edit N --repo owner/repo --add-label council-reviewed`
- `gh pr review N --repo owner/repo --approve` (or `--request-changes`) — formal review state
