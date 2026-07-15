#!/usr/bin/env bash
set -euo pipefail

if [[ $EUID -ne 0 ]]; then
    echo "Run this installer as root" >&2
    exit 1
fi

SOURCE_ENV="${1:-/home/nichlas/euthergate-turn.env}"
CADDY_CANDIDATE="${2:-/home/nichlas/euthergate-caddy-turn.Caddyfile}"
PUBLIC_IP="${EUTHERGATE_TURN_PUBLIC_IP:-90.235.24.7}"
PRIVATE_IP="${EUTHERGATE_TURN_PRIVATE_IP:-192.168.32.186}"
GATE_PEER_IP="${EUTHERGATE_GATE_PEER_IP:-192.168.32.88}"
REALM="${EUTHERGATE_TURN_REALM:-euthergate.apothictech.se}"

if [[ ! -r "$SOURCE_ENV" ]]; then
    echo "Missing private TURN environment file: $SOURCE_ENV" >&2
    exit 1
fi
set -a
# shellcheck disable=SC1090
source "$SOURCE_ENV"
set +a
if [[ -z "${EUTHERGATE_TURN_SHARED_SECRET:-}" ]]; then
    echo "EUTHERGATE_TURN_SHARED_SECRET is missing" >&2
    exit 1
fi
if [[ ! -r "$CADDY_CANDIDATE" ]]; then
    echo "Missing validated Caddy candidate: $CADDY_CANDIDATE" >&2
    exit 1
fi

export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y coturn miniupnpc

install -d -m 750 -o root -g turnserver /etc/coturn

write_config() {
    local path="$1"
    local listen_port="$2"
    local relay_min="$3"
    local relay_max="$4"
    local transport_option="$5"
    umask 077
    {
        printf 'listening-ip=%s\n' "$PRIVATE_IP"
        printf 'relay-ip=%s\n' "$PRIVATE_IP"
        printf 'external-ip=%s/%s\n' "$PUBLIC_IP" "$PRIVATE_IP"
        printf 'listening-port=%s\n' "$listen_port"
        printf 'min-port=%s\n' "$relay_min"
        printf 'max-port=%s\n' "$relay_max"
        printf 'realm=%s\n' "$REALM"
        printf 'server-name=%s\n' "$REALM"
        printf 'use-auth-secret\n'
        printf 'static-auth-secret=%s\n' "$EUTHERGATE_TURN_SHARED_SECRET"
        printf 'fingerprint\n'
        printf 'stale-nonce=600\n'
        printf 'log-file=stdout\nsimple-log\n'
        printf 'no-tls\nno-dtls\nno-cli\nno-multicast-peers\nno-loopback-peers\n'
        # The browser allocation only needs to reach the EutherGate WebRTC
        # peer. Deny every other destination so a leaked one-hour credential
        # cannot turn this relay into a path to the rest of the LAN/internet.
        printf 'allowed-peer-ip=%s-%s\n' "$GATE_PEER_IP" "$GATE_PEER_IP"
        printf 'denied-peer-ip=0.0.0.0-255.255.255.255\n'
        [[ -n "$transport_option" ]] && printf '%s\n' "$transport_option"
    } > "$path"
    chown root:turnserver "$path"
    chmod 640 "$path"
}

write_config /etc/coturn/euthergate-3478.conf 3478 49160 49179 ""
write_config /etc/coturn/euthergate-443-udp.conf 443 49180 49199 no-tcp

install -m 644 /dev/stdin /etc/systemd/system/euthergate-turn-3478.service <<'UNIT'
[Unit]
Description=EutherGate TURN relay on TCP/UDP 3478
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=turnserver
Group=turnserver
ExecStart=/usr/bin/turnserver -c /etc/coturn/euthergate-3478.conf --pidfile=
Restart=on-failure
RestartSec=2
NoNewPrivileges=true
PrivateTmp=true
ProtectHome=true
ProtectSystem=strict

[Install]
WantedBy=multi-user.target
UNIT

install -m 644 /dev/stdin /etc/systemd/system/euthergate-turn-443-udp.service <<'UNIT'
[Unit]
Description=EutherGate TURN relay on UDP 443
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=turnserver
Group=turnserver
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE
ExecStart=/usr/bin/turnserver -c /etc/coturn/euthergate-443-udp.conf --pidfile=
Restart=on-failure
RestartSec=2
NoNewPrivileges=true
PrivateTmp=true
ProtectHome=true
ProtectSystem=strict

[Install]
WantedBy=multi-user.target
UNIT

timestamp="$(date +%Y%m%d%H%M%S)"
cp -a /etc/caddy/Caddyfile "/etc/caddy/Caddyfile.pre-euthergate-turn-$timestamp"
install -m 644 "$CADDY_CANDIDATE" /etc/caddy/Caddyfile
if ! caddy validate --config /etc/caddy/Caddyfile; then
    cp -a "/etc/caddy/Caddyfile.pre-euthergate-turn-$timestamp" /etc/caddy/Caddyfile
    echo "Caddy validation failed; restored previous configuration" >&2
    exit 1
fi

systemctl disable --now coturn.service 2>/dev/null || true
systemctl daemon-reload
systemctl reload caddy
systemctl enable --now euthergate-turn-3478.service euthergate-turn-443-udp.service

install -m 600 -o root -g root "$SOURCE_ENV" /etc/coturn/euthergate-turn.env

euthernet_root=/home/nichlas/EutherNet
if [[ -r "$euthernet_root/deploy/euthergate-turn-restart" && -r "$euthernet_root/deploy/euthernet-restart.sudoers" ]]; then
    visudo -cf "$euthernet_root/deploy/euthernet-restart.sudoers"
    install -m 755 -o root -g root "$euthernet_root/deploy/euthergate-turn-restart" /usr/local/sbin/euthergate-turn-restart
    install -m 440 -o root -g root "$euthernet_root/deploy/euthernet-restart.sudoers" /etc/sudoers.d/euthernet-restart
fi

echo "TURN services installed; configure NAT mappings for 443/udp, 3478/tcp+udp and 49160-49199/udp"
