# EutherBrowse first slice

## Goal

Add `OPEN BROWSER` beside `OPEN TERMINAL` in Forge. Each click opens a new
Firefox window in the persistent headless Forge Sway session. Firefox windows
appear as browser chips beside tmux session chips and can be selected without
stopping the terminal sessions.

## First-slice architecture

- Firefox runs at home inside the existing `euthergate-forge.service` Sway
  compositor.
- A private Firefox profile lives outside the repository under
  `~/.local/share/euthergate/browser/firefox-profile`.
- The initial URL is fixed to `https://chatgpt.com/`.
- Each Firefox top-level window gets its own Sway workspace and browser chip.
- Selecting a chip focuses and fullscreens that exact Sway container.
- The browser view reuses the authenticated `WORK · HTTPS/WSS` JPEG and input
  bridge against the Forge output.
- tmux sessions and Firefox windows have separate backend lifetimes but share
  one workspace strip in the web UI.

## Security boundary

- No OpenAI cookie or Firefox profile data is copied into EutherGate.
- Firefox and Sway control remain local to the user runtime directory.
- No Firefox remote-debugging port is opened in this slice.
- Application-level sideloaded extensions are excluded from the dedicated
  profile; only extensions explicitly installed into that profile are enabled.
- Browser APIs require the existing EutherGate authentication.
- The fixed start URL avoids accepting arbitrary launch commands or shell
  fragments from the browser.
- The existing single-viewer desktop guard also protects the browser stream.

## Deferred

- EutherID step-up before opening the first browser.
- Closing windows from a chip.
- Address bar controls and a configurable allowlist.
- Clipboard/file upload affordances specific to the browser view.
- WebRTC browser transport; the first slice deliberately uses the already
  validated restrictive-network HTTPS/WSS path.
