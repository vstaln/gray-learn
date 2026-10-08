<p align="center">
  <img src="assets/gray-logo.svg" alt="gray" width="96">
</p>
<h1 align="center">gray-learn</h1>
<p align="center">The agent learns — non-obvious lessons saved by the model, injected into future sessions.</p>
<p align="center">
  <a href="https://github.com/vstaln/gray-learn/blob/main/LICENSE"><img alt="MIT License" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
  <img alt="gray plugin" src="https://img.shields.io/badge/gray-plugin-7aa2f7.svg">
  <img alt="rust" src="https://img.shields.io/badge/built%20with-rust-orange.svg">
</p>

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

---

Part of the [gray](https://github.com/vstaln/gray) plugin ecosystem —
the open-source AI agent harness. <https://gray.alignment.id>
