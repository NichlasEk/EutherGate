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

The WSS/WebRTC input helper starts each `wtype` text command with the same 100 ms
startup barrier already used by EutherBrowse URL navigation, then waits for the
command to finish. `wtype -` was rejected after live measurement: it retained
text until stdin reached EOF, which made typed content appear only when a viewer
or session switch closed the helper. Consecutive queued text events are combined
into bounded batches before the synchronous command.

While an EutherBrowse stream is live, its hardware-keyboard listener remains
active for the whole browser view. It does not depend on the stream wrapper
remaining `document.activeElement` after a remote click or image refresh.
The `KEYBOARD` action also exposes a small `TYPE HERE → FIREFOX` field that uses
ordinary browser input events as an explicit fallback. Gate acknowledges only
the input event type, never its text; the UI shows `KEYBOARD ACTIVE` without
logging an email address or password.
Printable Windows desktop input is forwarded directly from `beforeinput.data`,
the same event stage that already made Backspace reliable. It does not depend
on a later textarea `input` event being emitted with a retained value.

`PASTE IMAGE` reads PNG, JPEG or WebP from the client clipboard, writes it to the
selected Forge Wayland clipboard through the existing authenticated 8 MiB
bridge, then injects Ctrl+V into the already focused Firefox field. If direct
Clipboard API access is blocked, the same button changes to `SELECT IMAGE` for
an explicit local file choice. The payload is not added to the Firefox profile
or Git; it remains owned by the Wayland clipboard until replaced. A delayed
refresh makes asynchronously decoded image previews visible.

Firefox on the headless pixman/Sway output can accept text without publishing a
new visible framebuffer until its fullscreen surface is reconfigured. Direct
`grim` hashes confirmed the capture itself remained byte-identical until a
fullscreen disable/enable cycle. WSS input therefore schedules one such repaint
80 ms after the first pending text or special-key event. Repaint requests are
coalesced to at most roughly 12 per second. Because text injection now waits for
`wtype` to finish, the repaint cannot race ahead of delivery to Firefox.

The same stale framebuffer can otherwise hide asynchronous page changes after
the last input, including a streaming ChatGPT answer. Browser activity therefore
arms a bounded three-minute live-refresh window. While the Gate tab is visible,
one coalesced Sway repaint is requested every 900 ms; leaving EutherBrowse or
letting the window expire stops the timer. The fullscreen disable/enable pair is
sent in one Sway IPC command to avoid spawning two helper processes per pulse.

Firefox 152 on the virtual pixman output otherwise blocklists both hardware
compositing and ordinary WebRender and falls back to Basic compositing. Live
measurement showed ChatGPT's message controls updating while newly streamed
Markdown remained blank in that mode. EutherBrowse therefore enables Firefox's
CPU-backed software WebRender (`SWGL`) in its private profile and launch
environment. An isolated profile was verified through `about:support` as
`Compositing: WebRender (Software)` before enabling it for the persistent
profile.

Mouse-wheel input uses Wayland's discrete wheel request, which carries both the
continuous axis value and a physical wheel step. Firefox can ignore a plain
continuous axis event when the source is declared as a physical wheel; the
discrete step preserves the Windows wheel direction and makes both chat-history
directions actionable.

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
