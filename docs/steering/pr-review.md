# PR Review — Output Format

> **Source of truth moved.** The complete, current review steering (protocol +
> output format, reconciled with the live templates: read-only self-fetch,
> `--body-file`, `--edit-last`, `[done]`) now lives in
> [`skills/pr-review/SKILL.md`](../../skills/pr-review/SKILL.md). This file is kept
> for reference; prefer the skill.


## Reviewer output

Post one message with all findings, then react 🆗.

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

### 🟢 F3: <title>
...

</details>

Verdict: **approve** | **request changes**
```

### Severity levels

| Level | Emoji | Meaning | Action |
|-------|-------|---------|--------|
| 🔴 Critical | `🔴` | Correctness bug, security issue, data loss | Must fix before merge |
| 🟡 Minor | `🟡` | Style, naming, defense-in-depth, non-blocking | Should fix, not a blocker |
| 🟢 Info | `🟢` | Praise, context, future consideration | No action needed |

### Rules

- Every finding cites `path/file:line` and quotes the relevant code
- Number findings (F1, F2, ...) so the chair can reference them
- Cover: correctness, security, design, test coverage
- If assigned an angle (via preset), focus on that angle only
- One message, no progress updates

## Chair synthesis output

Wait for quorum (all reviewers 🆗), then synthesize and post to the PR.

```
<VERDICT> — <one-sentence summary>

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
<merged description from reviewers, cite who raised it>

**Fix:**
```suggestion
<concrete fix>
```

### 🟡 F2: <title>
...

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

- 🔴 Critical: N
- 🟡 Minor: N
- 🟢 Info: N
- Reviewers: rev1 (approve), rev2 (request changes)
- Consensus: **approve** | **request changes** | **split**

</details>
```

### Verdict line

First line of the synthesis. One of:

- `LGTM ✅` — no critical findings, approve
- `CHANGES REQUESTED ⚠️` — has 🔴 findings, request changes

### Chair PR actions (via `gh`)

After posting the synthesis comment:

1. `gh pr comment <N> --repo <owner>/<repo>` — post the synthesis
2. `gh pr edit <N> --add-label council-reviewed` — mark reviewed
3. `gh pr review <N> --approve` or `--request-changes` — set review state
