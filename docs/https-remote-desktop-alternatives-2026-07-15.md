# EutherGate alternatives when WebRTC is blocked

## Situation

The work network allows the authenticated EutherGate application over HTTPS,
but blocks WebRTC and every tested direct/TURN candidate. The current
`WORK · HTTPS/WSS` compatibility path works, but it captures and sends a full
JPEG frame up to 12 times per second. It also starts a new `grim` process for
each frame. That makes it a useful compatibility baseline, not the intended
high-performance path.

All experiments below must remain behind the existing EutherOxide admin
authentication and same-origin HTTPS proxy. None of the backend protocols may
be exposed directly to the internet or described as a different protocol to
bypass an explicit network policy.

## Priority 1 — WayVNC/RFB over authenticated WSS

Run WayVNC against the selected Forge Wayland output on a private Unix socket.
A noVNC client embedded in the existing desktop view connects to an authenticated
EutherGate WebSocket, and the Rust gateway proxies that socket to the private
RFB listener.

Why this is first:

- it uses the HTTPS/WSS path already known to work at the company;
- RFB can send changed rectangles and copy operations instead of an entire
  JPEG for every frame;
- WayVNC attaches directly to a wlroots/Sway session and supplies native
  virtual pointer and keyboard input;
- noVNC already handles browser input, scaling, clipboard and multiple RFB
  encodings;
- the experiment can be an isolated protocol-picker choice without changing
  WebRTC or the JPEG fallback.

Security boundary:

- WayVNC binds only to an ephemeral private Unix socket;
- EutherGate starts it only for an authenticated, active desktop viewer;
- the existing one-viewer limit applies;
- the gateway terminates WayVNC when the WebSocket closes;
- no public VNC or websockify listener is created.

Acceptance checks:

1. `WORK · VNC/WSS` appears only when the WayVNC executable is available.
2. The browser receives a real framebuffer and pointer/keyboard input works.
3. Closing or switching the viewer stops the private WayVNC child.
4. WebRTC and `WORK · HTTPS/WSS` still pass their existing builds/tests.
5. On the work network, compare idle, typing and scrolling for at least two
   minutes and record responsiveness plus sanitized route information.

## Priority 2 — Apache Guacamole over HTTPS

Use a local VNC or RDP backend through `guacd`, with the Guacamole JavaScript
client tunneled through EutherGate. Guacamole supports WebSocket and a normal
HTTP tunnel, so this is the next candidate if the company proxy also degrades
or blocks WebSockets.

Trade-offs: it is mature and explicitly designed as a browser remote-desktop
gateway, but adds a daemon, protocol translation and a larger integration and
authentication surface than option 1.

## Priority 3 — Persistent video stream over WSS

Keep the existing authenticated WebSocket but replace per-frame `grim` JPEGs
with persistent wlroots screencopy and a video or damage-aware encoder. This is
likely the best custom EutherGate performance path, but is an optimization of
the existing transport rather than another protocol.

Candidate forms are low-latency H.264/VP8 chunks decoded in the browser or
damage rectangles encoded as WebP/JPEG. Codec support, buffering behavior and
recovery after lost frames must be tested in the actual company browser.

## Not prioritized

- **WebTransport/HTTP/3:** uses QUIC/UDP and is likely to fail on the same
  restrictive network as the blocked WebRTC UDP candidates.
- **Native RDP, SPICE or RustDesk ports:** require a native client or additional
  non-HTTPS ports that the work network is unlikely to allow.
- **HLS/DASH screen streaming:** broadly proxy-friendly, but normal segment
  buffering makes interactive desktop latency worse.

## Work-network result log

Record the eventual result in
[`network-transport-test-log.md`](network-transport-test-log.md) using the same
two-minute video and input procedure as the WebRTC/TURN profiles.
