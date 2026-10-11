<div align="center">
  <img alt="gray-learn" src="assets/icon.svg" width="120" height="120" />
  <h1>gray-learn</h1>
  <p><strong>The agent learns: non-obvious lessons it saves are injected into future sessions.</strong></p>
  <p>
    <a href="https://gray.alignment.id">Website</a> ·
    <a href="https://gray.alignment.id/plugins/gray-learn">Store</a> ·
    <a href="https://github.com/vstaln/gray-learn">Source</a> ·
    <a href="https://github.com/vstaln/gray">gray</a>
  </p>
  <p>
    <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-1c1c20?style=flat-square&labelColor=0a0a0b" /></a>
    <a href="https://www.rust-lang.org"><img alt="Built with Rust" src="https://img.shields.io/badge/built%20with-rust-1c1c20?style=flat-square&labelColor=0a0a0b&logo=rust&logoColor=d4a373" /></a>
    <a href="https://gray.alignment.id/plugins/gray-learn"><img alt="gray plugin" src="https://img.shields.io/badge/gray-plugin-1c1c20?style=flat-square&labelColor=0a0a0b&color=7aa2f7" /></a>
  </p>
</div>

<br/>

```bash
gray plugin install gray-learn
```

The agent learns: non-obvious lessons saved by the model, injected into
future sessions.

A sidecar plugin for [gray](https://github.com/vstaln/gray), scaffolded by
[gray-account](https://github.com/vstaln/gray-account).

## What it does

- `learn` tool — the model saves a one-line lesson: verified build/test
  quirks, user preferences, pitfalls, non-obvious codebase facts. Written
  as `- {text}` to `<gray-home>/learn/global.md` (scope `"global"`) or
  `<gray-home>/learn/projects/<sha1(cwd)[:12]>.md` (scope `"project"`,
  the default). Identical lines are deduped. Secrets and obvious code
  facts are explicitly out of scope.
- `prompt/context` — injects both files (project first) capped at ~4KB
  under a `## Learned lessons` header, plus a line telling the model it
  may call `learn` to save new discoveries. Nothing saved → nothing
  injected.
- `/learn` — numbered list of both files. `/learn forget <n>` removes
  lesson n from the project file. `/learn clear [project|global|all]`
  wipes lessons. `/learn distill` returns a prompt asking the model to
  review the session and save what's worth keeping.

State lives under `~/.gray/learn/` (`$GRAY_HOME` honored first).

## Wire methods

`plugin/manifest` · `tool/call` (`learn`) · `prompt/context` (hook) ·
`command/run` (`/learn`) · `plugin/shutdown`

## Install

```sh
gray plugin install learn
```

No host capabilities required — pure filesystem state.

## Develop

```sh
cargo test
gray account check      # entry point + manifest handshake
gray account publish    # check → build → release → publish to the gray registry
```

Bump `version` in `Cargo.toml` before each `publish`; the registry refuses to
republish a version.

## Folded in: gray-harness

Lessons now live in a continual-harness store (`src/mods/harness.rs`) — a
mechanics port of [prime-agent](https://github.com/PrimeIntellect-ai/prime-agent)'s
HarnessState + refinement loop (MIT-licensed; no prime-agent code is copied):

- The `learn` tool is unchanged — it writes memory-kind entries into the
  kinded store (`~/.gray/harness/{state.json,local/<cwd-hash>.json}`). Kinds:
  `prompt`, `memory`, `skill`, `subagent`. Existing `~/.gray/learn/*.md`
  files are imported once and renamed `*.imported`.
- `harness_*` tools: upsert/get/delete/list/search/overview plus
  `harness_record_refinement` and `harness_plan_refinement` (the planner
  runs through `host/run` when the `host.turn` capability is granted).
- `prompt/context` still injects "## Learned lessons" and now appends a
  bounded "## Harness" digest of the other kinds.
- `/harness` — list/show/delete/refine/rollback/status/on|off; auto-refine
  arms on `turn_end` every 25 turns with a 20-minute cooldown, gated by a
  review gate, recorded with inverse edits for `rollback`.

## Tags

`gray` `plugin` `learn` `rust`

---

Part of the [gray](https://github.com/vstaln/gray) plugin ecosystem —
the open-source AI agent harness. <https://gray.alignment.id>
