#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
MODE=create
if [[ "${1:-}" == "--replace-host" ]]; then
    MODE=replace
    shift
fi
TURN_HOST="${1:-}"

if [[ -z "$TURN_HOST" ]]; then
    echo "Usage: $0 [--replace-host] <public TURN hostname or IPv4 address>" >&2
    exit 2
fi
if [[ ! "$TURN_HOST" =~ ^[A-Za-z0-9.-]+$ || "$TURN_HOST" == -* || "$TURN_HOST" == *..* ]]; then
    echo "TURN host is not a valid hostname or IPv4 address" >&2
    exit 2
fi

is_ipv4=false
if [[ "$TURN_HOST" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]]; then
    is_ipv4=true
    IFS=. read -r -a octets <<< "$TURN_HOST"
    for octet in "${octets[@]}"; do
        if (( 10#$octet > 255 )); then
            echo "TURN host is not a valid IPv4 address" >&2
            exit 2
        fi
    done
fi

cd "$ROOT_DIR"
umask 077
if [[ "$MODE" == create && -r .env.turn ]]; then
    echo ".env.turn already exists; leaving its secret unchanged"
    exit 0
fi

if [[ "$MODE" == replace ]]; then
    if [[ ! -r .env.turn ]]; then
        echo ".env.turn does not exist; create it before replacing its host" >&2
        exit 2
    fi
    secret="$(sed -n 's/^EUTHERGATE_TURN_SHARED_SECRET=//p' .env.turn)"
    if [[ ! "$secret" =~ ^[A-Za-z0-9_-]+$ ]]; then
        echo ".env.turn does not contain the expected generated shared secret" >&2
        exit 2
    fi
else
    secret="$(python -c 'import secrets; print(secrets.token_urlsafe(48))')"
fi

if $is_ipv4; then
    turn_urls="turn:$TURN_HOST:443?transport=udp,turn:$TURN_HOST:3478?transport=udp,turn:$TURN_HOST:3478?transport=tcp"
else
    turn_urls="turns:$TURN_HOST:443?transport=tcp,turn:$TURN_HOST:443?transport=udp,turn:$TURN_HOST:3478?transport=udp,turn:$TURN_HOST:3478?transport=tcp"
fi

temporary=".env.turn.tmp.$$"
trap 'rm -f "$temporary"' EXIT
{
    printf 'EUTHERGATE_TURN_URLS=%s\n' "$turn_urls"
    printf 'EUTHERGATE_TURN_SHARED_SECRET=%s\n' "$secret"
} > "$temporary"
mv "$temporary" .env.turn
chmod 600 .env.turn
if [[ "$MODE" == replace ]]; then
    echo "Updated the private TURN host in $ROOT_DIR/.env.turn; shared secret preserved"
else
    echo "Created private TURN client configuration in $ROOT_DIR/.env.turn"
fi
