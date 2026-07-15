#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
runtime_dir="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
session_dir="$runtime_dir/euthergate-forge"
session_file="$session_dir/session.env"

install -d -m 700 "$session_dir"
rm -f "$session_file"

export XDG_RUNTIME_DIR="$runtime_dir"
export XDG_CURRENT_DESKTOP=sway
export XDG_SESSION_DESKTOP=euthergate-forge
export XDG_SESSION_TYPE=wayland
export WLR_BACKENDS=headless
export WLR_LIBINPUT_NO_DEVICES=1
export WLR_RENDERER=pixman

sway --unsupported-gpu --config "$ROOT_DIR/config/forge-sway.conf" &
sway_pid=$!

cleanup() {
    rm -f "$session_file"
    if kill -0 "$sway_pid" 2>/dev/null; then
        kill "$sway_pid" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

sway_socket="$runtime_dir/sway-ipc.$(id -u).$sway_pid.sock"
for _ in $(seq 1 100); do
    if [[ -S "$sway_socket" ]]; then
        wayland_socket="$(find "$runtime_dir" -maxdepth 1 -type s -name 'wayland-*' -newer "$session_dir" -printf '%f\n' 2>/dev/null | sort | tail -1)"
        if [[ -n "$wayland_socket" ]]; then
            printf 'BACKEND=sway\nWAYLAND_DISPLAY=%s\nSWAYSOCK=%s\nOUTPUT=HEADLESS-1\n' \
                "$wayland_socket" "$sway_socket" > "$session_file"
            chmod 600 "$session_file"
            wait "$sway_pid"
            exit $?
        fi
    fi
    if ! kill -0 "$sway_pid" 2>/dev/null; then
        wait "$sway_pid"
        exit $?
    fi
    sleep 0.05
done

echo "Forge compositor did not publish its Wayland and IPC sockets" >&2
exit 1
