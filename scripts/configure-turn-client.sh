#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
TURN_HOST="${1:-}"

if [[ -z "$TURN_HOST" ]]; then
    echo "Usage: $0 <public TURN hostname or IP>" >&2
    exit 2
fi
if [[ "$TURN_HOST" == *[[:space:],/]* ]]; then
    echo "TURN host contains unsupported characters" >&2
    exit 2
fi

cd "$ROOT_DIR"
umask 077
if [[ -r .env.turn ]]; then
    echo ".env.turn already exists; leaving its secret unchanged"
    exit 0
fi

secret="$(python -c 'import secrets; print(secrets.token_urlsafe(48))')"
{
    printf 'EUTHERGATE_TURN_URLS=turn:%s:443?transport=udp,turn:%s:3478?transport=udp,turn:%s:3478?transport=tcp\n' "$TURN_HOST" "$TURN_HOST" "$TURN_HOST"
    printf 'EUTHERGATE_TURN_SHARED_SECRET=%s\n' "$secret"
} > .env.turn
chmod 600 .env.turn
echo "Created private TURN client configuration in $ROOT_DIR/.env.turn"
