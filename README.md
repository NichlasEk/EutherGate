# EutherGate

EutherGate is a secure browser gate to a remote development machine: persistent
terminal sessions first, then files, builds, Codex and streamed Wayland apps.

The first two checkpoints form a usable vertical slice:

- token login with an HttpOnly, SameSite cookie;
- a real PTY-backed shell in the browser;
- one persistent terminal session that survives browser reloads;
- resize support and a bounded output replay buffer;
- a small health/status API for automation.
- a selectable physical or headless Hyprland output;
- VP8 desktop video transported with WebRTC;
- pointer, keyboard and wheel events over a WebRTC DataChannel;
- a browser switcher between Gate Shell and Remote Forge.

## Run it

Requirements: Rust, Node.js and npm.

Remote Forge additionally needs an active Hyprland session, `grim`, Python with
PyGObject and `websockets`, and the GStreamer WebRTC and VP8 plugins.
On Arch Linux these are supplied by `gstreamer`, `gst-plugins-bad`,
`gst-plugins-good`, `gst-libav`, `gst-python`, `python-websockets` and `grim`.

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
Select **DESKTOP**, choose an output, then **START DESKTOP**. A physical output
such as `DP-1` shows its current Hyprland workspace and existing windows.
`EUTHERGATE-1` creates an isolated virtual output for remote-only work. This is
Wayland output/workspace selection inside the current graphical session, not a
switch between Linux virtual terminals (TTYs).

Use **OPEN TERMINAL** to launch Kitty directly on the selected output's active
workspace. The equivalent command from Gate Shell is, for example:

```bash
hyprctl dispatch exec '[workspace 3 silent] kitty'
```

The workspace number is shown in the lower-left WebRTC HUD and may differ from
`3` depending on the current compositor state.

Click the streamed desktop to enter remote control. The browser locks the
pointer for relative movement; press **Esc** to leave remote control and return
the host cursor to the position it had before control began.

With the gateway running, an optional end-to-end reconnect smoke test is:

```bash
EUTHERGATE_TOKEN=your-token python scripts/smoke_terminal.py
```

It requires the Python `websockets` package.

The full media and input smoke test requires a running gateway and active
Hyprland session:

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
UI and release binary, and enables `euthergate.service` plus
`euthergate-tunnel.service`. The tunnel exposes the gateway only as
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
| `EUTHERGATE_SECURE_COOKIE` | `false` | Add `Secure` to the auth cookie. Enable behind HTTPS. |
| `EUTHERGATE_PROXY_TOKEN` | unset | Shared secret accepted only from the EutherOxide admin proxy. |
| `RUST_LOG` | `euthergate=info,tower_http=info` | Log filter. |

Never expose this checkpoint directly to the public internet. Put it behind TLS
and a trusted access layer. A VPN such as Tailscale is currently the simplest
remote path because ICE only advertises host candidates; internet traversal
still needs configurable STUN/TURN. The next security slice will also add
durable users, session isolation and explicit reverse-proxy trust.

Remote Forge currently captures through one `grim` process per frame and
injects input through Hyprland IPC. This proves the complete browser-to-Wayland
path, but the next performance slice should replace capture with persistent
wlroots screencopy buffers and replace IPC input with the advertised virtual
keyboard/pointer Wayland protocols.

See [docs/architecture.md](docs/architecture.md) for the system direction.
