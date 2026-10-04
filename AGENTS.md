# AGENTS.md

Guidance for coding agents working in this repository.

## Project shape

- This is a rustc-only tmux utility, not a Cargo project.
- Rust sources live in `src/` with `src/main.rs` as the entrypoint.
- Bun end-to-end tests live in `tests/` and drive isolated tmux servers.
- `tmux-overview.sh` is the user-facing build/run wrapper and must keep working without Cargo.

## Constraints

- Do not add Cargo files unless explicitly requested.
- Preserve existing tmux behavior and keybindings unless the task asks for a behavior change.
- Keep generated binaries, caches, and local build output out of git.
- If paths move, update all of:
  - `tmux-overview.sh`
  - `tests/e2e.test.ts`
  - `README.md`

## Build and validation

Use the Makefile as the canonical entry point so builds always go through the user-facing wrapper:

```sh
make build
make check
```

`make build` calls `./tmux-overview.sh --build-only`, which uses local `rustc` or falls back to Docker. `make check` validates the wrapper, builds through it, and runs the isolated E2E suite. Do not invoke `rustc` or the Docker build directly unless debugging the wrapper itself.

The E2E suite requires `bun`, `tmux`, and either `rustc` or `docker`.

## Style

- Keep modules small and focused.
- Prefer standard library Rust only.
- Keep shell scripts `#!/usr/bin/env bash` with `set -euo pipefail`.
- Use exact, user-visible README commands that can be run from the repo root.
