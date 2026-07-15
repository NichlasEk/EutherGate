# EutherGate protocol picker handoff — 2026-07-15

## Goal

Expose a browser-local protocol picker for the EutherGate desktop so restrictive
networks can be tested route by route without restarting the gateway. The
terminal is separate: it uses WebSocket over HTTPS/WSS and is not affected by
the desktop protocol selection.

## Current state

- The protocol picker implementation is present in the working tree and built
  into the release frontend/backend.
- The picker offers `AUTO`, `DIRECT / LAN`, `WORK · TURN/TLS 443`,
  `TURN/UDP 443`, `TURN/TCP 3478` and `TURN/UDP 3478` when the matching TURN
  URLs are configured.
- Changing the picker reconnects only the WebRTC desktop viewer. It does not
  restart EutherGate, Forge or the reverse tunnel.
- The selection is stored in browser local storage per browser/device.
- The authenticated desktop API was smoke-tested and returned all six
  profiles.
- Rust tests pass: 11 passed, 0 failed.
- `cargo fmt --check`, `git diff --check`, the TypeScript check, Vite production
  build and `cargo build --release` completed successfully.

## HTTPS/WSS compatibility fallback

After every advertised TURN candidate failed on the work network, an explicit
`WORK · HTTPS/WSS` profile was added. It uses the authenticated
`/ws/desktop-fallback` application route rather than ICE/TURN. The helper sends
JPEG frames at up to 12 fps and accepts the existing pointer and keyboard JSON
events over the same WebSocket. `AUTO` remains the default WebRTC profile.

The local end-to-end smoke test against the real Forge output received a
complete 143312-byte JPEG frame and successfully wrote input messages. Rust
tests, Python compilation/probe, TypeScript checking, the Vite production build
and the release Rust build passed. The running gateway must use the new release
binary before the work-network test.

## Work-network finding

The work computer reported this sanitized browser error for the TLS profile:

```text
TURN ERROR · turns:turn.90-235-24-7.sslip.io:443/tcp · 701
```

The work network explicitly blocks `sslip.io` DNS. The UDP 443 and TCP/UDP 3478
profiles failed silently. Error 701 is consistent with no local ICE candidate
being able to reach the configured TURN server.

## DNS change completed

An explicit Cloudflare DNS record was created:

```text
turn.apothictech.se  A  90.235.24.7  DNS only
```

Both Cloudflare DNS (`1.1.1.1`) and Google DNS (`8.8.8.8`) returned
`90.235.24.7` after the change. Do not proxy this record through the normal
Cloudflare HTTP proxy; TURN requires the DNS-only origin address.

An internal TLS probe against the public address saw the ZTE router certificate.
This is assumed to be NAT hairpin behavior. External TURN/TLS still needs to be
validated from the work network after activation.

## Prepared but not active

The ignored private file `.env.turn` has been updated with
`scripts/configure-turn-client.sh --replace-host turn.apothictech.se`. The
existing shared secret was preserved. It now advertises:

```text
turns:turn.apothictech.se:443?transport=tcp
turn:turn.apothictech.se:443?transport=udp
turn:turn.apothictech.se:3478?transport=udp
turn:turn.apothictech.se:3478?transport=tcp
```

EutherGate reads `.env.turn` only at process startup. The active process still
advertised the old `sslip.io` TLS URL at the end of this session.

Snapshot before restart:

```text
euthergate.service        active/running since 2026-07-15 13:50:38 CEST
euthergate-tunnel.service active/running since 2026-07-15 13:35:06 CEST
```

No restart was performed after preparing the new hostname because restarting
`euthergate.service` terminates the current EutherGate terminal connection.

## Resume procedure

1. Read this file and inspect `git status --short --branch`. The protocol picker
   changes were still uncommitted when this handoff was written; preserve them.
2. Confirm `.env.turn` contains `turn.apothictech.se` without printing its
   shared-secret line.
3. At a user-approved interruption point, restart only the gateway:

   ```bash
   systemctl --user restart euthergate.service
   ```

   Do not restart the tunnel or Forge unless their status independently shows a
   problem.
4. Reconnect through EutherOxide and verify:

   ```bash
   systemctl --user status euthergate.service euthergate-tunnel.service --no-pager
   ```

5. Authenticated `/api/desktop/status` must show
   `turns:turn.apothictech.se:443?transport=tcp` and must not show `sslip.io`.
6. On the work computer, select `WORK · TURN/TLS 443`, wait for `LIVE`, and
   verify video, pointer input and one harmless key press for at least two
   minutes.
7. Record the sanitized `ICE ROUTE` or `TURN ERROR` in
   `docs/network-transport-test-log.md`.

If TURN still reports 701, select `WORK · HTTPS/WSS`. Its footer should show
`STREAM ROUTE · authenticated HTTPS/WSS`; verify video and input for at least
two minutes and record the result in the same test log.

## Known access detail

The normal administrative SSH keys tried against `nichlas@192.168.32.186` were
rejected with `Permission denied (publickey)`, and password authentication was
disabled. The dedicated reverse-tunnel key remained connected and
`euthergate-tunnel.service` stayed active. Server-side certificate/Caddy work,
if needed after the external test, requires restored administrative SSH access.
