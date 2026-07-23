# EutherBrowse stability slice

## Incident

The first live browser session could browse successfully, but switching between
Firefox and the Forge terminal could leave the web view looking frozen. The
local pointer also became difficult to see after the browser entered pointer
lock.

Logs ruled out an OOM, Firefox crash and Forge restart. The failure was a viewer
lifecycle problem:

- terminal and desktop shared one frontend WebSocket variable;
- a new browser view could reconnect before the old single-viewer lease cleared;
- a half-open HTTPS/WSS connection had no application heartbeat;
- an outgoing JPEG send could wait indefinitely on a client that stopped
  reading;
- pointer lock hid the local cursor;
- cleanup reset video state before all pending mouse releases were sent.

The Codex process remained alive in the persistent `gate` tmux session through
the incident and service restart.

## Changes

- Terminal signaling and desktop signaling now have separate WebSocket
  lifecycles.
- View changes release mouse/key state before closing the desktop transport.
- Output, protocol and browser switches poll the authenticated desktop status
  until the old viewer is actually gone.
- The UI waits up to 23 seconds, covering the server's stale-viewer lease,
  instead of relying on a fixed 250 ms delay.
- HTTPS/WSS sends a JSON heartbeat every five seconds.
- A viewer with no application message for 20 seconds is released.
- A blocked JPEG WebSocket send is bounded to three seconds.
- HTTPS/WSS reconnects up to three times with bounded exponential backoff.
- Viewer connect, reject and disconnect events are visible in the Gate log.
- EutherBrowse keeps absolute pointer control and draws a local high-contrast
  cursor instead of entering pointer lock.
- `OPEN BROWSER` reuses an existing Firefox window. New windows require the
  explicit `NEW WINDOW` action.
- The active Firefox window can be closed from `CLOSE WINDOW`.
- The Firefox-window limit is reduced from eight to four.

## Verification

`scripts/smoke_wss_viewer_lifecycle.py` exercises the real Forge output through
an isolated Gate instance. It verifies:

1. the second simultaneous viewer is rejected;
2. a clean disconnect releases the lease;
3. a replacement viewer immediately receives JPEG frames;
4. a healthy viewer remains connected for more than the heartbeat timeout;
5. a client that stops reading and sending is released without restarting Gate.

The normal Rust unit suite, release build and TypeScript/Vite production build
must also pass before deployment.

## Deferred

- Replace per-frame `grim` process creation with a persistent capture/encoder
  pipeline if CPU/process-launch profiling shows it is worthwhile.
- Add per-device viewer ownership if simultaneous desktop control from several
  authenticated devices becomes a desired workflow.
