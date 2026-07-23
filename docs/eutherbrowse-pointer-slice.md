# EutherBrowse pointer and window controls

## Incident

EutherBrowse showed a clear lime cursor and the cursor followed the local mouse,
but small controls in an existing Firefox window could not be clicked reliably.
The visible cursor was a frontend overlay. Forge's headless Sway seat reported
`capabilities: 0`, so the old `swaymsg seat seat0 cursor ...` input path did not
provide Firefox with a real pointer device.

The broad click test was misleading because it did not prove that movement,
button state and the target coordinate reached Firefox as one pointer stream.

## Changes

- `euthergate-pointer` is a small Wayland client using
  `zwlr_virtual_pointer_v1`.
- The Python WebRTC and HTTPS/WSS bridges keep one virtual pointer alive for the
  viewer lifetime and send absolute movement, buttons and wheel events through
  it.
- The helper releases every held button before disconnecting.
- The web view captures a mouse pointer from down through up, tracks held
  buttons, and releases them on cancellation, blur or view disposal.
- Browser-mode movement is forwarded as soon as the local pointer enters the
  frame. It no longer waits for a first click while only the lime overlay moves.
- Each Firefox session chip has its own small close button. It closes exactly
  that Firefox window without first activating it.
- Closing the active Firefox window is transactional: Gate first confirms the
  exact window was closed, then releases the viewer and opens the next session.
  A failed close leaves the existing viewer connected and usable instead of
  replacing it with a dead error screen.
- The browser UI closes windows through a POST action endpoint. The older DELETE
  endpoint remains available, but POST also works through restrictive workplace
  reverse proxies that reject or drop DELETE requests.
- The WSS smoke test can opt into a real click and explicit coordinates with
  `EUTHERGATE_SMOKE_CLICK=1`, `EUTHERGATE_SMOKE_X` and
  `EUTHERGATE_SMOKE_Y`.

## Verification

With the virtual-pointer process alive, Sway reported:

```text
capabilities: 1
identifier: 0:0:wlr_virtual_pointer_v1
type: pointer
```

A temporary Firefox event page recorded `CLICK_800_500` from the Rust helper and
`CLICK_640_400` through the real Python `InputController`. The temporary window,
HTTP server, cookie and screenshots were removed afterwards, and focus was
returned to the persistent ChatGPT window on Forge workspace 11.

A post-deploy regression test then recorded `CLICK_639_313` through the complete
live HTTPS/WSS pipeline. A separate live test closed only temporary Firefox
window 28 with HTTP 204 while the WSS viewer was still connected. The two user
Firefox windows on workspaces 10 and 11 remained open.

The normal Rust tests, Python syntax checks and TypeScript/Vite production build
must pass before the Gate restart.
