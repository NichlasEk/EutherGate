# EutherGate TURN relay

EutherGate uses coturn when a remote browser cannot establish a direct ICE
path, which is common on corporate networks. The browser obtains a one-hour
TURN REST credential from the authenticated desktop status endpoint. The
long-lived shared secret is never included in the web bundle or repository.

## Deployed topology

- Public address: `90.235.24.7`
- TURN listeners: UDP `443`, TCP/UDP `3478`
- Relay range: UDP `49160-49199`
- Origin host: `192.168.32.186`
- EutherGate source host: `192.168.32.88`
- TURN peer traffic is allowlisted to `192.168.32.88`; authenticated relay
  allocations cannot be used to reach other LAN or internet destinations.
- Caddy keeps TCP `443`; HTTP/3 is disabled to reserve UDP `443` for TURN.
- `play.apothictech.se` is Cloudflare-proxied and must not be used as a TURN
  address. The current UDP routes use the public IP directly, while the TCP/TLS
  route uses a temporary `sslip.io` hostname for certificate name matching.

The ZTE router mappings are created with `upnpc` running on `192.168.32.186`;
the router rejects attempts from another LAN client to map ports to the relay.
They are expected to use a permanent lease (`0`).

## Secret handling

The private local file `.env.turn` is ignored by Git and loaded by
`scripts/run-service.sh`. A root-owned copy lives at
`/etc/coturn/euthergate-turn.env` on the relay host. Both contain the same
`EUTHERGATE_TURN_SHARED_SECRET`; do not print or commit it.

To create the initial file without printing the secret:

```bash
./scripts/configure-turn-client.sh <public-turn-host-or-ip>
```

## First-party TURN hostname

`turn.apothictech.se` is the intended replacement for the temporary `sslip.io`
name. It must be a DNS-only A record pointing directly to `90.235.24.7`; a
normal Cloudflare HTTP proxy does not carry TURN. Do not activate the hostname
until all of these checks pass:

```bash
getent ahosts turn.apothictech.se
openssl s_client -connect turn.apothictech.se:443 -servername turn.apothictech.se -brief </dev/null
```

The TLS certificate must name `turn.apothictech.se`, and the TCP/TLS frontend
must still forward TURN connections to coturn. Once that is true, preserve the
existing shared secret and replace only the client hostname:

```bash
./scripts/configure-turn-client.sh --replace-host turn.apothictech.se
```

Restarting `euthergate.service` is deliberately a separate step because it
terminates every terminal and desktop connection hosted by the gateway.

## Services and verification

```bash
systemctl status euthergate-turn-3478.service euthergate-turn-443-udp.service
ss -lntup | rg ':3478|:443|:491[6-9][0-9]'
journalctl -u euthergate-turn-3478.service -u euthergate-turn-443-udp.service
```

EutherGate `/api/desktop/status` should return a non-empty `ice_servers` array
to authenticated clients. The web UI reports gathered candidates, sanitized
TURN endpoints, ICE errors and the selected route. It never displays TURN
usernames, credentials or candidate IP addresses.

## Protocol picker

The authenticated desktop menu builds its choices from transport profiles
returned by the server. Profiles only appear when their matching TURN URL is
configured. The choices are:

- `AUTO`: direct WebRTC plus every configured relay;
- `DIRECT / LAN`: direct WebRTC without a TURN server;
- `WORK · HTTPS/WSS`: JPEG frames and input over the authenticated web path;
- `WORK · VNC/WSS`: WayVNC/RFB over an authenticated same-origin WebSocket;
- `WORK · TURN/TLS 443`: relay-only TLS over TCP 443;
- `TURN/UDP 443`: relay-only UDP over port 443;
- `TURN/TCP 3478`: relay-only TCP over the standard TURN port;
- `TURN/UDP 3478`: relay-only UDP over the standard TURN port.

Changing the profile closes the current viewer and starts the selected WebRTC
or WSS transport. It does not restart the remote compositor or gateway. The
selected profile is saved in browser local storage, so a work computer can
retain a different choice from a phone or home computer. The TURN choices are
ordinary advertised TURN transports; none is presented as HTTP or disguised as
another protocol.

The HTTPS/WSS profile is the explicit exception to the WebRTC routes. It uses
`/ws/desktop-fallback` on the already authenticated application origin, sends
binary JPEG frames at up to 12 fps, and returns the normal pointer/keyboard JSON
events over the same socket. It is a compatibility path for restrictive
networks, not an attempt to label TURN as HTTP. The shared one-viewer limit also
applies across WebRTC and WSS viewers.

Use [network-transport-test-log.md](network-transport-test-log.md) to record
which profile works on each network.

## Rotation and rollback

To rotate the shared secret, create a new `.env.turn`, copy it to the relay,
rerun the installer, then restart `euthergate.service`. Existing allocations
remain valid only until their one-hour credentials expire.

The installer stores a timestamped Caddy backup at
`/etc/caddy/Caddyfile.pre-euthergate-turn-*`. To remove TURN, stop and disable
the two `euthergate-turn-*` services, remove the router mappings, restore the
latest Caddy backup and reload Caddy.
