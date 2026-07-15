#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

set -a
# shellcheck disable=SC1091
source .env
set +a

runtime_dir="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
if [[ -z "${HYPRLAND_INSTANCE_SIGNATURE:-}" ]]; then
    socket="$(find "$runtime_dir/hypr" -mindepth 2 -maxdepth 2 -name .socket.sock -printf '%T@ %h\n' 2>/dev/null | sort -nr | head -1 | cut -d' ' -f2-)"
    if [[ -n "$socket" ]]; then
        export HYPRLAND_INSTANCE_SIGNATURE="${socket##*/}"
    fi
fi
if [[ -z "${WAYLAND_DISPLAY:-}" ]]; then
    wayland_socket="$(find "$runtime_dir" -maxdepth 1 -type s -name 'wayland-*' -printf '%T@ %f\n' 2>/dev/null | sort -nr | head -1 | cut -d' ' -f2-)"
    export WAYLAND_DISPLAY="${wayland_socket:-wayland-1}"
fi

exec "$ROOT_DIR/target/release/euthergate"
