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

## Validation

Prefer these checks before committing:

```sh
docker run --rm \
  -v "$PWD:/src:ro" \
  -v /tmp/tmux-overview-build:/out \
  -w /src \
  rust:1-alpine \
  rustc -C debuginfo=0 /src/src/main.rs -o /out/tmux-overview

bun test tests/e2e.test.ts
```

The e2e suite requires `bun`, `tmux`, and either `rustc` or `docker`. Local `rustc` is not guaranteed to be installed on the host; use the Docker command above to build this Rust tool when it is unavailable.

## Style

- Keep modules small and focused.
- Prefer standard library Rust only.
- Keep shell scripts `#!/usr/bin/env bash` with `set -euo pipefail`.
- Use exact, user-visible README commands that can be run from the repo root.
