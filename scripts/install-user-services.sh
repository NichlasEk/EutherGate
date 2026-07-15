#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
cd "$ROOT_DIR"

for command in sway swaymsg wtype grim; do
    if ! command -v "$command" >/dev/null; then
        echo "Missing required Forge command: $command" >&2
        echo "On Arch Linux: sudo pacman -S sway wtype grim" >&2
        exit 1
    fi
done

umask 077
touch .env
if ! grep -Eq '^EUTHERGATE_TOKEN=.+$' .env; then
    printf 'EUTHERGATE_TOKEN=%s\n' "$(python -c 'import secrets; print(secrets.token_urlsafe(32))')" >> .env
fi
if ! grep -Eq '^EUTHERGATE_PROXY_TOKEN=.+$' .env; then
    printf 'EUTHERGATE_PROXY_TOKEN=%s\n' "$(python -c 'import secrets; print(secrets.token_urlsafe(48))')" >> .env
fi
if ! grep -Eq '^EUTHERGATE_BIND=.+$' .env; then
    printf 'EUTHERGATE_BIND=127.0.0.1:8787\n' >> .env
fi
chmod 600 .env

if [[ ! -d web/node_modules ]]; then
    npm --prefix web ci
fi
npm --prefix web run build
cargo build --release

install -d -m 700 "$UNIT_DIR"
install -m 644 "$ROOT_DIR/deploy/systemd/euthergate.service" "$UNIT_DIR/euthergate.service"
install -m 644 "$ROOT_DIR/deploy/systemd/euthergate-tunnel.service" "$UNIT_DIR/euthergate-tunnel.service"
install -m 644 "$ROOT_DIR/deploy/systemd/euthergate-forge.service" "$UNIT_DIR/euthergate-forge.service"
systemctl --user daemon-reload
systemctl --user enable euthergate-forge.service euthergate.service euthergate-tunnel.service

# Pick up updated binaries, scripts, and unit files even when an older version
# of the services was already active before this installation.
systemctl --user restart euthergate-forge.service
systemctl --user restart euthergate.service
systemctl --user restart euthergate-tunnel.service

if [[ "$(loginctl show-user "$USER" -p Linger --value 2>/dev/null || true)" != "yes" ]]; then
    echo
    echo "Forge works now, but pre-login boot requires lingering:"
    echo "  sudo loginctl enable-linger $USER"
fi
