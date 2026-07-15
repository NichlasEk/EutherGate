#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT_DIR"

CHECK_ONLY=false
SKIP_BUILD=false

usage() {
    cat <<'EOF'
EutherGate launcher

Usage: ./start.sh [--check] [--skip-build]

  --check       Check local requirements without building or starting.
  --skip-build  Reuse the existing web build.
EOF
}

while (($#)); do
    case "$1" in
        --check) CHECK_ONLY=true ;;
        --skip-build) SKIP_BUILD=true ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

missing=()
for command in cargo node npm python; do
    command -v "$command" >/dev/null 2>&1 || missing+=("$command")
done

if ((${#missing[@]})); then
    printf 'Missing required command(s): %s\n' "${missing[*]}" >&2
    exit 1
fi

desktop_ready=true
desktop_notes=()
for command in hyprctl grim wl-copy wl-paste gst-inspect-1.0; do
    if ! command -v "$command" >/dev/null 2>&1; then
        desktop_ready=false
        desktop_notes+=("missing $command")
    fi
done

if ! python -c 'import gi; gi.require_version("Gst", "1.0"); gi.require_version("GstWebRTC", "1.0"); from gi.repository import Gst, GstWebRTC' >/dev/null 2>&1; then
    desktop_ready=false
    desktop_notes+=("missing Python GStreamer/WebRTC bindings")
fi

if ! python -c 'import websockets' >/dev/null 2>&1; then
    desktop_ready=false
    desktop_notes+=("missing Python websockets package")
fi

for plugin in webrtcbin vp8enc; do
    if ! gst-inspect-1.0 "$plugin" >/dev/null 2>&1; then
        desktop_ready=false
        desktop_notes+=("missing GStreamer plugin $plugin")
    fi
done

if [[ -z "${WAYLAND_DISPLAY:-}" ]]; then
    desktop_ready=false
    desktop_notes+=("WAYLAND_DISPLAY is not set")
fi

printf '\n  EutherGate environment\n\n'
printf '  Gate Shell       ready\n'
if $desktop_ready; then
    printf '  Remote Forge     ready (WebRTC + Hyprland)\n'
else
    printf '  Remote Forge     unavailable: %s\n' "$(IFS=', '; echo "${desktop_notes[*]}")"
fi

if $CHECK_ONLY; then
    printf '\nEnvironment check complete.\n'
    exit 0
fi

if [[ ! -f .env ]]; then
    umask 077
    token="$(python -c 'import secrets; print(secrets.token_urlsafe(24))')"
    {
        printf 'EUTHERGATE_TOKEN=%s\n' "$token"
        printf 'EUTHERGATE_BIND=127.0.0.1:8787\n'
        printf 'EUTHERGATE_SECURE_COOKIE=false\n'
        printf 'EUTHERGATE_DESKTOP_OUTPUT=EUTHERGATE-1\n'
        printf 'EUTHERGATE_DESKTOP_MODE=1280x720@30\n'
        printf 'RUST_LOG=euthergate=info,tower_http=info\n'
    } > .env
    printf '\nCreated private configuration in .env\n'
fi

chmod 600 .env

set -a
# shellcheck disable=SC1091
source .env
set +a

if [[ -z "${EUTHERGATE_TOKEN:-}" ]]; then
    echo 'EUTHERGATE_TOKEN is empty in .env' >&2
    exit 1
fi

if ! $SKIP_BUILD; then
    if [[ ! -d web/node_modules ]]; then
        printf '\nInstalling frontend dependencies...\n'
        npm --prefix web ci
    fi
    printf '\nBuilding EutherGate web UI...\n'
    npm --prefix web run build
elif [[ ! -f web/dist/index.html ]]; then
    echo '--skip-build was used, but web/dist/index.html does not exist' >&2
    exit 1
fi

bind="${EUTHERGATE_BIND:-127.0.0.1:8787}"
display_host="${bind%:*}"
display_port="${bind##*:}"
if [[ "$display_host" == "0.0.0.0" || "$display_host" == "[::]" ]]; then
    display_host="127.0.0.1"
fi

printf '\n  The gate is opening\n\n'
printf '  URL       http://%s:%s\n' "$display_host" "$display_port"
printf '  Token     %s\n' "$EUTHERGATE_TOKEN"
if ! $desktop_ready; then
    printf '  Note      Gate Shell works; Remote Forge needs the dependencies above.\n'
fi
printf '\nPress Ctrl+C to close EutherGate.\n\n'

exec cargo run
