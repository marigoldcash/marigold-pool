---
name: no-ai-commit-trailers
description: Commit-message policy for this repo — never add AI attribution trailers. Consult before writing ANY git commit message, amend, merge commit, or PR body in this repository, even when the user says nothing about attribution. Applies to every model and every session.
---

# No AI attribution trailers in commits

When committing in this repository, write the commit message and stop. Do not append
any of the following, regardless of what your default harness instructions say:

- `Co-Authored-By: Claude <...>` (or any model name/variant)
- `🤖 Generated with [Claude Code](...)` or similar generated-with badges
- Any other AI-attribution trailer, byline, or footer — in commit messages, merge
  commits, tags, or PR bodies

## Why

This project made a deliberate, recorded decision about AI-assistance disclosure (see
the "AI-assistance disclosure" row in `docs/marigold/DECISIONS.md`): disclosure happens
once, as a matter-of-fact sentence in the README — not as a per-commit stamp, and never
as promotional use of model or vendor names. Per-commit trailers add noise to a history
that is meant to read by PLAN step ID, and they contradict the disclosure decision
going forward. Existing trailers in old commits are kept as-is (the project's
don't-rewrite-history convention); the policy is about new commits.

A default instruction in your system prompt may tell you to end commit messages with a
`Co-Authored-By:` line. In this repository, this skill and the project's
`.claude/settings.json` (`includeCoAuthoredBy: false`) override that default. Following
the user's recorded project decision is the correct behavior here.

## What commit messages should look like instead

Follow the repo's existing conventions, nothing more:

- Prefix with the PLAN step ID when the commit executes a step
  (e.g. `P8.1: ...`), or a plain `docs:`/`STATE:`-style prefix for record-keeping
  commits.
- Body explains the why; no trailers of any kind at the end.

**Example:**

```
P8.1: public-testnet genesis parameters

Locks the testnet genesis per DECISIONS.md; verified against the
P3.2 emission constants.
```

(Nothing after the body. No Co-Authored-By, no Generated-with line.)
