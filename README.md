# tmux-overview

A small rustc-only tmux session/window overview popup.

![tmux-overview screenshot](assets/tmux-overview.png)

## Features

- Lists tmux sessions and windows, with `s` toggling CPU/RSS stats.
- Nests slash-named sessions (`project/worker`) under their live parent session.
- Draws a togglable tree (`t`) with white session edges and gray window branches.
- Hides internal `__` helper sessions until `.` toggles them visible, rendering friendly `◫`/`󰚩` labels.
- Live pane-title/window labels.
- Preview pane with multi-pane layout rendering.
- Save, restore, rename, delete, and kill session/window flows.
- Renaming a session also renames every slash-named descendant.
- Add windows/sessions from the popup (`a`, then `name` for a window or `s:name` for a session).
- Watchlist section for pinned windows (`w` toggles the selected window).
- Bright badges for agent panes that stopped without being reviewed.
- Persisted session fold state, preview visibility/size, and cursor position.
- No Cargo project required; builds directly with `rustc`, falling back to Docker.

## Prerequisites

Runtime:

- `tmux`
- `ps`, `stty`, `date`, and a POSIX-ish shell environment

Build, choose one:

- `rustc`
- or `docker` for the wrapper's fallback build path, using `rust:1-alpine` by default

Tests:

- `bun`
- `tmux`
- `rustc` or `docker`

## Install

Clone somewhere under your tmux config, for example:

```sh
git clone https://github.com/bymoses/tmux-overview ~/.config/tmux/tmux-overview
```

Add a binding to `~/.config/tmux/tmux.conf`:

```tmux
bind-key s display-popup -E -w 90% -h 90% -T "overview" "~/.config/tmux/tmux-overview/tmux-overview.sh"
bind-key -n M-s display-popup -E -w 90% -h 90% -T "overview" "~/.config/tmux/tmux-overview/tmux-overview.sh"
```

Reload tmux config, then press prefix + `s` or Alt+`s`.

## Agent alerts

Windows whose agent stopped, but that you have not looked at since, are badged
in the picker: `● 4m` on the window row (`◌` when the agent exited, `●2` when
two panes stopped), and `● N` on every ancestor session row, so a collapsed or
`__`-hidden `__agents` session still surfaces its pending work.

A producer rings the terminal bell when it stops, and `window_bell_flag` stays
set until the window is focused. Space can also manually mark or clear an alert
without switching windows; viewing the window clears the manual mark. A pane counts as an agent when it publishes
`@agent_state` (`idle`, `exited`, `blocked`, `working`) as a tmux pane option:

```sh
tmux set-option -p -t "$TMUX_PANE" @agent_state idle
tmux set-option -p -t "$TMUX_PANE" @agent_idle_at "$(date +%s)"
printf '\a' > /dev/tty
```

`@agent_idle_at` (epoch seconds) drives the age shown in the badge. Panes that
only ring the bell still light up if their command is a known agent (`pi`,
`claude`, `codex`, `aider`), so producers need no integration at all.

## Subscription limits (agents mode)

`A` shows pi agents and a bottom line with each account's cached limits.
`7d 79/43 ↻3d24h` means 79% used, 43% elapsed (linear use), and time to reset.
Colours match pi-kit's status line; observations older than 30 minutes are dim with `?`.
At narrow widths reset times yield first, keeping all accounts visible.
Caches are read from `${XDG_RUNTIME_DIR:-/tmp}/pi-usage/` (`PI_USAGE_CACHE_DIR` overrides).
`pi-kit usage --poll` runs in the background at most every five minutes; it must be on `PATH`.
Set `TMUX_OVERVIEW_USAGE_POLL=off` for cache-only operation. Missing caches hide the line.

## Session naming and keys

| Key | Action |
| --- | --- |
| Space | Toggle unseen on an agent or window; clear a project or session subtree |

- `parent/child` appears beneath a live `parent` session; each level folds independently.
- Components beginning with `__` are hidden by default. Press `.` to toggle them; legacy symbol-marked helpers remain recognized.
- tmux keeps the real names `<session>/__float` and `<session>/__agents`; the overview displays those components as `◫ float` and `󰚩 agents`.
- Press `s` to toggle compact and CPU/RAM stats views.
- Press `t` to toggle tree connectors.
- Hidden-session visibility, tree view, and stats mode persist across overview openings.

## Layouts

`S` saves the selected session; `L` saves the whole server. The **Layouts** section
shows saved times and pane counts. Enter restores missing sessions without touching
existing ones; `D` deletes a selected snapshot. Background `__bg` sessions are excluded.

```sh
./tmux-overview.sh save-all
./tmux-overview.sh restore                    # newest snapshot, or legacy import
./tmux-overview.sh restore /path/snapshot.tsv
./tmux-overview.sh --server isolated restore /path/snapshot.tsv --fork
```

`--server` accepts a tmux `-L` socket name or an absolute `-S` socket path.
Snapshots live in `${XDG_STATE_HOME:-$HOME/.local/state}/tmux-overview/layouts/`:
newest 20 plus the newest snapshot per UTC day for seven days. Writes are atomic.
Single-session saves use the same pi-aware v2 format in `sessions/`; old v1 saves
and `~/.local/state/tmux-snapshot.tsv` remain readable without modification.

Pi panes restore through pi-kit's `bin/pi-doze-start.sh`, without starting pi.
`PI_DOZE_START` overrides its path. Enter or an inbox message wakes the saved
conversation; `--fork` makes the first wake fork it instead. Worker parent pane
ids are remapped after all panes exist. Missing/already-open pi logs get a shell,
never an empty conversation. Old v1 saves cannot recover pi conversation identity.
Missing cwds fall back to the nearest existing parent (then `$HOME`).
`nvim`/`yazi` relaunch; other programs get a shell. `PI_*` identity is scrubbed.

The opt-in real-snapshot test uses a separate socket/runtime, forks on wake,
and removes its test server/runtime/fork afterwards:

```sh
systemd-run --user --scope -p MemoryMax=4G -p TasksMax=500 -p CPUQuota=300% \
  env TMUX_OVERVIEW_REAL_SNAPSHOT="$HOME/.local/state/tmux-snapshot.tsv" \
  bun test tests/restore-real.test.ts
```

## Build/run

Use the Makefile so builds go through the same wrapper used at runtime. The wrapper writes the binary to `${XDG_CACHE_HOME:-$HOME/.cache}/tmux-overview/tmux-overview` when sources change:

```sh
make build
make run
```

`make build` calls `./tmux-overview.sh --build-only` and automatically uses local `rustc` or falls back to Docker.

## Tests

End-to-end tests are Bun-based and drive isolated tmux servers. Run the complete project check with:

```sh
make check
```
