# EutherBrowse stability restart handoff — 2026-07-23

## Scope

This deploy replaces the fragile Firefox/terminal viewer handoff with separate
frontend sockets, explicit viewer release, HTTPS/WSS heartbeat and send
timeouts, retry, a visible local cursor, browser reuse and browser-window
closing.

The pre-deploy live revision is `fd5df4c`.

## Files

- `src/main.rs`
- `web/src/main.ts`
- `web/src/style.css`
- `scripts/smoke_wss_viewer_lifecycle.py`
- `docs/eutherbrowse-first-slice.md`
- `docs/eutherbrowse-stability-slice.md`
- this handoff

## Pre-restart evidence

- `cargo test`: 19 passed.
- TypeScript/Vite production build: passed.
- Isolated real-Forge lifecycle smoke: passed viewer exclusion, clean
  replacement, 24 seconds of healthy heartbeats and stale-viewer release.
- No live service was restarted during diagnosis or isolated verification.

## Deployment

Build the release binary and web bundle, then restart only the Gate service:

```sh
cargo build --release --bin euthergate
(cd web && npm run build)
systemd-run --user --collect --on-active=2s \
  --unit=euthergate-browser-stability-restart \
  /usr/bin/systemctl --user restart euthergate.service
```

Do not restart Forge, tmux or the tunnel.

## Post-restart checks

```sh
systemctl --user is-active \
  euthergate.service euthergate-forge.service \
  euthergate-tmux.service euthergate-tunnel.service
curl --fail --silent http://127.0.0.1:8787/api/health
```

Then verify authenticated browser sessions, a real HTTPS/WSS JPEG frame and the
new viewer connect/disconnect log entries. The existing Firefox profile and
windows should survive the Gate restart.

## Completed live verification

- Gate restarted at 10:11:08 CEST as PID 2065707.
- Gate, Forge, tmux and the tunnel remained active.
- Both existing ChatGPT windows survived on workspaces 10 and 11.
- Live HTTPS/WSS returned a 61,305-byte JPEG and accepted input.
- `viewer_connected` returned to `false` immediately after the smoke.
- The live log recorded the new `https-wss` connect and disconnect events.
- A newly created third Firefox window was closed through the authenticated
  `DELETE` API; the two pre-existing windows remained untouched.
