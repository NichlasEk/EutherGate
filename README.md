# EutherGate

EutherGate is a secure browser gate to a remote development machine: persistent
terminal sessions first, then files, builds, Codex and streamed Wayland apps.

The first checkpoint is deliberately narrow and already useful:

- token login with an HttpOnly, SameSite cookie;
- a real PTY-backed shell in the browser;
- one persistent terminal session that survives browser reloads;
- resize support and a bounded output replay buffer;
- a small health/status API for automation.

## Run it

Requirements: Rust, Node.js and npm.

```bash
cp .env.example .env
npm --prefix web install
npm --prefix web run build
set -a; source .env; set +a
cargo run
```

Open `http://127.0.0.1:8787` and enter the token from `.env`.

With the gateway running, an optional end-to-end reconnect smoke test is:

```bash
EUTHERGATE_TOKEN=your-token python scripts/smoke_terminal.py
```

It requires the Python `websockets` package.

For frontend work, run the gateway and Vite separately:

```bash
cargo run
npm --prefix web run dev
```

Vite proxies `/api` and `/ws` to the gateway.

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `EUTHERGATE_TOKEN` | generated at startup | Login credential. Set this outside local development. |
| `EUTHERGATE_BIND` | `127.0.0.1:8787` | Gateway listen address. |
| `EUTHERGATE_SHELL` | `$SHELL`, then `/bin/sh` | Shell started inside the PTY. |
| `EUTHERGATE_WORKDIR` | current directory | Initial directory for the shell. |
| `EUTHERGATE_WEB_ROOT` | `web/dist` | Built frontend directory. |
| `EUTHERGATE_SECURE_COOKIE` | `false` | Add `Secure` to the auth cookie. Enable behind HTTPS. |
| `RUST_LOG` | `euthergate=info,tower_http=info` | Log filter. |

Never expose this checkpoint directly to the public internet. Put it behind TLS
and a trusted access layer. The next security slice will add durable users,
session isolation and explicit reverse-proxy trust.

See [docs/architecture.md](docs/architecture.md) for the system direction.
