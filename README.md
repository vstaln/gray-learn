# gray-learn

The agent learns: non-obvious lessons saved by the model, injected into
future sessions. Inspired by ECC's "instincts" and claude-mem.

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
