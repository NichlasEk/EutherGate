#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

set -a
# shellcheck disable=SC1091
source .env
set +a

runtime_dir="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
forge_session="$runtime_dir/euthergate-forge/session.env"
for _ in $(seq 1 100); do
    [[ -r "$forge_session" ]] && break
    sleep 0.05
done
if [[ -r "$forge_session" ]]; then
    while IFS='=' read -r key value; do
        case "$key" in
            WAYLAND_DISPLAY|SWAYSOCK) export "$key=$value" ;;
        esac
    done < "$forge_session"
    export XDG_CURRENT_DESKTOP=sway
    export XDG_SESSION_DESKTOP=euthergate-forge
    export XDG_SESSION_TYPE=wayland
fi

exec "$ROOT_DIR/target/release/euthergate"
