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

## Keyboard follow-up

Hardware keyboard input in EutherBrowse sends printable characters as the
character produced by the local browser rather than reconstructing them from
the physical `KeyboardEvent.code`. This preserves `@`, shifted punctuation,
Swedish characters and password symbols when the client and Forge keyboard
layouts differ. Navigation keys and explicit Ctrl/Alt/Meta shortcuts continue
through the key-event path.

The WSS/WebRTC input helper keeps one `wtype` text process alive for the viewer
lifetime. Starting a fresh virtual keyboard and immediately typing dropped its
first character before Sway had attached the device; because hardware input
previously started one process per key, that could drop every printable
character. The persistent process pays the attachment delay once. Standalone
special-key invocations use the same 100 ms startup barrier already used by
EutherBrowse URL navigation.

While an EutherBrowse stream is live, its hardware-keyboard listener remains
active for the whole browser view. It does not depend on the stream wrapper
remaining `document.activeElement` after a remote click or image refresh.
The `KEYBOARD` action also exposes a small `TYPE HERE → FIREFOX` field that uses
ordinary browser input events as an explicit fallback. Gate acknowledges only
the input event type, never its text; the UI shows `KEYBOARD ACTIVE` without
logging an email address or password.

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
