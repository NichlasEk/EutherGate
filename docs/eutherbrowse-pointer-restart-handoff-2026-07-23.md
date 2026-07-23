# EutherBrowse pointer restart handoff — 2026-07-23

## Scope

Deploy the real Forge virtual pointer, browser pointer capture and per-window
close buttons. Do not restart Forge, tmux or the tunnel.

## Persistent state

- The `gate` tmux session must remain attached and untouched.
- The two user Firefox windows on Forge workspaces 10 and 11 must remain open.
- The temporary click-test Firefox window and HTTP server have already been
  closed.
- The intended active Firefox window after verification is workspace 11.

## Pre-restart checks

```sh
cargo test
cargo build --release
python -m py_compile scripts/webrtc_desktop.py scripts/wss_desktop.py scripts/smoke_wss_desktop.py
(cd web && npm run build)
git diff --check
```

The release build must contain both:

```text
target/release/euthergate
target/release/euthergate-pointer
```

## Restart

Wait until `/api/desktop/status` reports `viewer_connected: false`. Then use a
delayed transient user unit to restart only `euthergate.service`, so the
terminal carrying out the deployment is not cut off in place.

## Post-restart checks

- `euthergate.service` is active with a new main PID.
- `GET /api/health` returns `{"status":"ok"}`.
- `euthergate-forge.service`, `euthergate-tmux.service` and
  `euthergate-tunnel.service` stayed active and were not restarted.
- Browser sessions on workspaces 10 and 11 still exist.
- A new WSS viewer receives a JPEG frame.
- A real pointer event is delivered through the virtual-pointer helper.
