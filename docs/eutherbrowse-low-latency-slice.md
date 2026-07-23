# EutherBrowse low-latency JPEG slice

## Problem

EutherBrowse could move and click its remote Firefox pointer, but the browser
felt increasingly slow. The HTTPS/WSS bridge continuously sent complete JPEG
frames without knowing whether the client had displayed them. A slow or
restrictive connection could therefore preserve old frames in the transport
queue. `grim -c` also embedded the compositor cursor in each JPEG, so the view
showed both an old remote cursor and the immediate lime DOM cursor.

## Bounded first fix

- Browser-mode WSS capture requests `cursor=hidden`; ordinary desktop WSS keeps
  the compositor cursor.
- The existing lime browser cursor remains local and responds immediately to
  pointer movement.
- A browser client sends `frame_ack` only after the JPEG image has loaded.
- While a frame is unacknowledged, Gate retains at most one pending JPEG and
  replaces it whenever a newer capture arrives.
- Status and input messages still travel immediately; only JPEG delivery is
  gated.

This deliberately keeps the existing JPEG/WSS transport. It removes stale-frame
backlog and duplicate cursor lag without introducing a new codec or changing
Firefox session ownership.

## Verification

The lifecycle smoke test now proves that a second JPEG cannot arrive before the
first has been acknowledged, then proves that delivery resumes after
`frame_ack`. It also continues to verify viewer exclusion, clean replacement,
healthy heartbeats and stale-viewer cleanup.

Before a live restart, run:

```text
python -m py_compile scripts/wss_desktop.py scripts/smoke_wss_desktop.py scripts/smoke_wss_viewer_lifecycle.py
cd web && npm run build
cargo test
cargo build --release
```

