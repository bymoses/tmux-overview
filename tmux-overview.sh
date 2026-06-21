#!/usr/bin/env bash
set -euo pipefail

src_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
src="$src_dir/src/main.rs"
cache_dir="${XDG_CACHE_HOME:-$HOME/.cache}/tmux-overview"
bin="$cache_dir/tmux-overview"
lock="$cache_dir/build.lock"
image="${TMUX_OVERVIEW_RUST_IMAGE:-rust:1-alpine}"

mkdir -p "$cache_dir"

needs_build() {
  [[ ! -x "$bin" ]] && return 0
  find "$src_dir/src" -type f -name '*.rs' -newer "$bin" -print -quit | grep -q .
}

build_with_local_rustc() {
  command -v rustc >/dev/null 2>&1 || return 1
  printf ':: building tmux-overview with local rustc\n'
  rustc -C opt-level=z -C strip=symbols "$src" -o "$tmp"
}

build_with_docker() {
  command -v docker >/dev/null 2>&1 || return 1
  printf ':: building tmux-overview with docker image %s\n' "$image"
  docker run --rm \
    --user "$(id -u):$(id -g)" \
    -v "$src_dir:/src:ro" \
    -v "$cache_dir:/out" \
    -w /src \
    "$image" \
    rustc -C opt-level=z -C strip=symbols /src/src/main.rs -o "/out/$(basename "$tmp")"
}

if needs_build; then
  exec 9>"$lock"
  if command -v flock >/dev/null 2>&1; then
    flock 9
  fi

  if needs_build; then
    tmp="$cache_dir/tmux-overview.$$"
    trap 'rm -f "$tmp"' EXIT

    if build_with_local_rustc || build_with_docker; then
      chmod +x "$tmp"
      mv -f "$tmp" "$bin"
      trap - EXIT
    else
      printf 'cannot build tmux-overview: neither local rustc nor docker is available\n'
      printf 'press any key to close...'
      IFS= read -rsn1 _ || true
      exit 1
    fi
  fi
fi

exec "$bin"
