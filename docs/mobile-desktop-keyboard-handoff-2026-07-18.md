# Mobile desktop keyboard handoff (2026-07-18)

## Implemented

- Desktop mode has an explicit `KEYBOARD` action that focuses a transient IME textarea.
- Committed Android IME text is forwarded as bounded `text` messages over WebRTC
  DataChannel or the HTTPS/WSS fallback and injected with `wtype`.
- Enter, Backspace, Delete, arrows, navigation keys, modifiers and F1-F12 remain
  key events.
- VNC/WSS translates the same mobile input to RFB keysyms, including Unicode.
- The textarea is cleared after each submitted text fragment and on desktop disposal.

## Verified before live activation

- `cd web && npm run build`
- `python -m py_compile scripts/webrtc_desktop.py scripts/wss_desktop.py`
- `cargo test` (15 passed)
- `git diff --check`

## Live activation

Inspect `euthergate.service`, the local health endpoint and the active desktop
transport before restart. Use a delayed user-service restart so the current Gate
request can finish, then reconnect and verify health before testing input.

## Phone smoke test

1. Open Desktop and wait for `LIVE`.
2. Tap `KEYBOARD` and choose EutherBoard if Android asks.
3. Type ordinary Swedish text, Space, period, Backspace and Enter.
4. Verify Ctrl/Alt, arrows and Esc in a remote terminal.
5. Repeat on the selected WebRTC, HTTPS/WSS or VNC/WSS profile as needed.
