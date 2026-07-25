# Code style

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

Conventions for rwv source that aren't enforced by clippy or rustfmt — read this
before writing or reviewing rwv source.

## No silent fallback on inconsistent state

Where state on disk or in config could be (a) absent/legitimate, (b)
present/valid, or (c) present/invalid, the read path must distinguish (b) from
(c) and surface (c) loudly. Silent coalescing of (c) into (a)'s default or (b)'s
happy path is a bug, not a defensible default.

**Patterns to avoid in rwv core** (not exhaustive — flag any analog in review):

- `unwrap_or(...)` / `unwrap_or_default()` / `unwrap_or_else(...)` where the default is reached on normal-state input and the downstream consumer cannot distinguish it from a correctly-computed value. Defaults are fine when the absence-of-state *is* the user's intent (optional config with documented default); they're a bug when the absence indicates broken state.
- `Result::ok()` / `.ok()` patterns that discard the `Err`. If the error path is unreachable, document why; if it's reachable on normal-state input, propagate or surface.
- `match x { Some(y) => …, None => fallback }` where `None` indicates corruption and `fallback` silently produces wrong output downstream.
- "Warn and proceed" — emitting a warning that has no enforceable downstream effect, especially when the warning is suppressed in `--json` mode.
- **In-memory backfill** of missing fields read from disk. The marker file is the authority; if it's incomplete, fix the file (`rwv doctor --fix`), don't fill in defaults at read time.
- **Tests asserting necessary-but-insufficient conditions** — e.g. asserting a sync moved the manifest tip without asserting the history shape that movement was supposed to produce. A prior attempt shipped semantically-wrong code that satisfied all tests; the tests had a blind spot.

## What to do instead

Make the read path return a `Result` that distinguishes the three cases (a/b/c above). Each callsite explicitly handles `(c)` — typically with `?` propagation or a `bail!` with an actionable error message. For state that may be encountered in many places (e.g., `.rwv-active`, `.rwv-workweave`), centralize the parse + validate logic so every callsite gets the same fail-loud treatment.

When the silent fallback is genuinely the user's intent (an optional knob), document the default in the field's doc comment and in [`reference/formats.md`](../reference/formats.md).

## Doctor coverage

Any new "fail-loud on inconsistent state" surface should have a corresponding `rwv doctor` violation kind, so an operator running doctor sees the issue before it bites in some other verb. If the inconsistent state is auto-fixable on disk (legacy marker migration, dangling-pointer cleanup), wire it into `rwv doctor --fix`. If not (e.g., broken YAML the operator must edit), surface it as a violation with no auto-fix.

## Origin

Surfaced during the 2026-05-27 review session. Five concrete instances had landed silently and were caught only via cross-cutting audit. The pattern was named and these guidelines added so the same shape isn't re-introduced.
