# EutherGate

EutherGate is a secure browser gate to a remote development machine: persistent
terminal sessions first, then files, builds, Codex and streamed Wayland apps.

The first two checkpoints form a usable vertical slice:

- token login with an HttpOnly, SameSite cookie;
- a real PTY-backed shell in the browser;
- one persistent terminal session that survives browser reloads;
- resize support and a bounded output replay buffer;
- a small health/status API for automation.
- a persistent pre-login Forge desktop plus selectable logged-in Hyprland outputs;
- VP8 desktop video transported with WebRTC;
- pointer, keyboard and wheel events over a WebRTC DataChannel;
- text and image clipboard transfer between the selected Wayland session and browser;
- authenticated physical display wake-up with a short idle grace period;
- a browser switcher between Gate Shell and Remote Forge.

## Run it

Requirements: Rust, Node.js and npm.

Remote Forge additionally needs Sway, `wtype`, `grim`, `wl-clipboard`, Python with PyGObject and
`websockets`, and the GStreamer WebRTC and VP8 plugins. A logged-in Hyprland
session is optional and appears automatically as another set of outputs.
On Arch Linux these are supplied by `gstreamer`, `gst-plugins-bad`,
`gst-plugins-good`, `gst-libav`, `gst-python`, `python-websockets`, `sway`,
`wtype`, `grim` and `wl-clipboard`.

The easy path checks the environment, creates a private token on first run,
builds the web UI and starts the gateway:

```bash
./start.sh
```

To only inspect requirements without starting anything:

```bash
./start.sh --check
```

The manual equivalent is documented below.

```bash
cp .env.example .env
npm --prefix web install
npm --prefix web run build
set -a; source .env; set +a
cargo run
```

Open `http://127.0.0.1:8787` and enter the token from `.env`.
Select **DESKTOP**, choose an output, then **START DESKTOP**. **Forge Session**
is a separate headless Sway compositor that can exist before graphical login.
After login, physical outputs such as `DP-1` and the optional logged-in virtual
output appear in the same picker. Switching changes compositor/output, not a
Linux virtual terminal (TTY).

Use **OPEN TERMINAL** to launch Kitty directly on the selected output's active
workspace. The equivalent command from Gate Shell is, for example:

```bash
hyprctl dispatch exec '[workspace 3 silent] kitty'
```

Use **CLIPBOARD** to move plain text, PNG, JPEG or WebP images in either
direction. **REMOTE → HERE** copies the selected Wayland session's current
clipboard to the browser computer. **HERE → REMOTE** reads the local browser
clipboard; when a browser blocks direct reads, focus the displayed paste box
and press Ctrl+V. Payloads are authenticated, never logged and limited to 8 MiB.

The workspace number is shown in the lower-left WebRTC HUD and may differ from
`3` depending on the current compositor state.

Use **WAKE SCREENS** to turn on the physical outputs of the logged-in Hyprland
session. EutherGate asks Hypridle to stay idle-inhibited for two minutes so a
locked screen does not immediately switch off again. Hyprlock remains locked;
the action never enters a password or unlocks the session.

The authenticated EutherOxide server map can also schedule restarts of the
gateway, reverse tunnel and persistent Forge compositor. The API accepts only
the fixed `gateway`, `tunnel` and `forge` service names; restart jobs are delayed
briefly so the proxy response can finish before a service or tunnel goes down.

Click the streamed desktop to enter remote control. The browser locks the
pointer for relative movement; press **Esc** to leave remote control and return
the host cursor to the position it had before control began.

With the gateway running, an optional end-to-end reconnect smoke test is:

```bash
EUTHERGATE_TOKEN=your-token python scripts/smoke_terminal.py
```

It requires the Python `websockets` package.

While using Gate Shell, paste a PNG, JPEG or WebP image directly into the
terminal with Ctrl+V. EutherGate stores it in a private per-user temporary
directory and inserts the absolute file path into the active terminal input, so
Codex or another terminal tool can open it. **PASTE IMAGE** is the file-picker
fallback for browsers that do not expose image clipboard data. Images are
limited to 8 MiB and are not written to the repository.

The full media and input smoke test requires a running gateway and Forge
session:

```bash
EUTHERGATE_TOKEN=your-token python scripts/smoke_webrtc.py
```

It starts the virtual output, negotiates WebRTC, decodes a real VP8 desktop
frame, opens the DataChannel and sends a pointer event.

For frontend work, run the gateway and Vite separately:

```bash
cargo run
npm --prefix web run dev
```

Vite proxies `/api` and `/ws` to the gateway.

## Autostart and EutherOxide tunnel

Install the release gateway and its persistent reverse tunnel as user services:

```bash
./scripts/install-user-services.sh
```

This generates a private `EUTHERGATE_PROXY_TOKEN` when needed, builds the web
UI and release binary, and enables `euthergate-forge.service`,
`euthergate.service` and `euthergate-tunnel.service`. With systemd user
lingering enabled, the Forge compositor, gateway and tunnel start at boot before
graphical login. The tunnel exposes the gateway only as
`127.0.0.1:18787` on the EutherOxide host; it does not bind a public server
port. EutherOxide must authenticate an admin request, strip the `/euthergate`
prefix, and add the configured token as `X-EutherGate-Proxy-Token` to HTTP and
WebSocket upstream requests.

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `EUTHERGATE_TOKEN` | generated at startup | Login credential. Set this outside local development. |
| `EUTHERGATE_BIND` | `127.0.0.1:8787` | Gateway listen address. |
| `EUTHERGATE_SHELL` | `$SHELL`, then `/bin/sh` | Shell started inside the PTY. |
| `EUTHERGATE_WORKDIR` | current directory | Initial directory for the shell. |
| `EUTHERGATE_WEB_ROOT` | `web/dist` | Built frontend directory. |
| `EUTHERGATE_DESKTOP_OUTPUT` | `EUTHERGATE-1` | Name of the headless Hyprland output. |
| `EUTHERGATE_DESKTOP_MODE` | `1280x720@30` | Virtual output resolution and frame rate. |
| `EUTHERGATE_DESKTOP_HELPER` | `scripts/webrtc_desktop.py` | GStreamer media helper. |
| `EUTHERGATE_WAYVNC_BIN` | discovered from `PATH` | Optional WayVNC executable for authenticated VNC/WSS. |
| `EUTHERGATE_FORGE_SESSION_FILE` | `$XDG_RUNTIME_DIR/euthergate-forge/session.env` | Runtime descriptor for the persistent Forge compositor. |
| `EUTHERGATE_TERMINAL_UPLOAD_DIR` | `/tmp/euthergate-<uid>/terminal-images` | Private directory for images pasted into Gate Shell. |
| `EUTHERGATE_SECURE_COOKIE` | `false` | Add `Secure` to the auth cookie. Enable behind HTTPS. |
| `EUTHERGATE_PROXY_TOKEN` | unset | Shared secret accepted only from the EutherOxide admin proxy. |
| `EUTHERGATE_TURN_URLS` | unset | Comma-separated TURN/TURNS relay URLs for restrictive remote networks. |
| `EUTHERGATE_TURN_SHARED_SECRET` | unset | Coturn REST shared secret used to mint one-hour client credentials. |
| `RUST_LOG` | `euthergate=info,tower_http=info` | Log filter. |

The desktop footer includes sanitized ICE diagnostics: candidate types,
transport, TURN endpoint errors and the selected route. Credentials and
candidate IP addresses are intentionally omitted. Prefer a dedicated,
DNS-only first-party hostname such as `turn.apothictech.se` for TURN/TLS; see
[docs/turn-relay.md](docs/turn-relay.md) for the staged migration checks.

The desktop menu also has a protocol picker. `AUTO` keeps direct WebRTC and all
configured relays available. The other choices constrain a new connection to a
single route such as relay-only TURN/TLS on TCP 443 or TURN/UDP on 443. Changing
the choice reconnects only the desktop viewer and stores the preference in that
browser. It does not change the setting for phones or other computers. Record
network-specific results in
[docs/network-transport-test-log.md](docs/network-transport-test-log.md).

`WORK · HTTPS/WSS` is the compatibility route for networks that allow the web
application but block every WebRTC/TURN candidate. It sends JPEG frames and
input through one authenticated WebSocket on the same HTTPS origin. This costs
more bandwidth and latency than VP8/WebRTC, so `AUTO` remains the default.

`WORK · VNC/WSS` appears when WayVNC is installed. The embedded noVNC client
speaks RFB over an authenticated same-origin WebSocket. EutherGate starts one
WayVNC child per viewer on a private Unix socket and stops it when the viewer
disconnects; no VNC TCP port is exposed. This route can use changed-region RFB
encodings and native Wayland input, making it the first performance alternative
to the full-frame JPEG fallback on HTTPS-only networks. While a VNC viewer is
connected, EutherGate wakes the selected output and holds an idle inhibitor so
Hyprland or Sway cannot pause capture by powering that output off.

Never expose this checkpoint directly to the public internet. Put it behind TLS
and a trusted access layer. A VPN such as Tailscale is currently the simplest
remote path when TURN is unavailable. Configure the TURN settings above for
internet traversal through restrictive networks. The next security slice will also add
durable users, session isolation and explicit reverse-proxy trust.

Remote Forge currently captures through one `grim` process per frame. Input is
injected through compositor-local Sway/Hyprland IPC plus `wtype`; it is not sent
to the physical greeter. This proves the complete browser-to-Wayland path, but
the next performance slice should replace capture with persistent wlroots
screencopy buffers and pointer IPC with the virtual-pointer Wayland protocol.

See [docs/architecture.md](docs/architecture.md) for the system direction.
