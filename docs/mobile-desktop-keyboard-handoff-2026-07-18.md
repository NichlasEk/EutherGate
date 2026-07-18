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

## Live activation result

No restart was required. EutherGate serves `web/dist` dynamically, and the
desktop helper is loaded when a desktop connection starts. All three Gate user
services were active, no desktop helper was running, the live service returned
`{"status":"ok"}`, and the served hashed JavaScript matched `web/dist` byte for
byte. The next Desktop session therefore picks up both frontend and helper
changes without interrupting the current Gate session.

## Mobile cache follow-up

The public tunnel was verified to serve the current `index-CUKbo67x.js`, while
an already open Android tab still displayed the previous action row without
`KEYBOARD`. Static UI responses now carry `Cache-Control: no-store` so reopening
or reloading Gate cannot keep a stale entry HTML or JavaScript bundle. Activating
that response-header change requires one gateway restart; the tunnel and Forge
services do not need to be restarted.

## Phone smoke test

1. Open Desktop and wait for `LIVE`.
2. Tap `KEYBOARD` and choose EutherBoard if Android asks.
3. Type ordinary Swedish text, Space, period, Backspace and Enter.
4. Verify Ctrl/Alt, arrows and Esc in a remote terminal.
5. Repeat on the selected WebRTC, HTTPS/WSS or VNC/WSS profile as needed.
