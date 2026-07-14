# EutherGate architecture

## Product direction

EutherGate is a remote development cockpit, not merely a remote desktop. A
person and Codex should be able to share a machine, terminal, repository and
visual result through one authenticated browser surface.

The intended path is:

1. **Gate Shell** — authenticated, reconnectable terminal sessions.
2. **Forge Workspace** — file tree, editor, diffs, builds and logs.
3. **Glass Stream** — WebRTC video for one Wayland application.
4. **Remote Seat** — keyboard, pointer and touch input over a data channel.
5. **Euther Desktop** — virtual outputs, audio, clipboard and resilient sessions.
6. **Codex Control** — build, launch, observe and iterate inside the same session.

## Checkpoint 1

```text
Browser/xterm
    | HTTPS + WebSocket
    v
EutherGate gateway
    | authenticated cookie
    | bounded replay buffer
    v
Persistent PTY + login shell
```

The gateway owns the terminal rather than the WebSocket. Closing or reloading a
browser therefore only disconnects a viewer; it does not terminate the shell.
On reconnection, the gateway sends recent buffered output before forwarding new
PTY output.

Checkpoint 1 intentionally has one shared terminal per gateway process. It is a
local-development vertical slice, not yet a multi-user security boundary.

## Security boundary

The browser never receives the configured login credential after authentication.
Successful login creates a random process-local session identifier in an
HttpOnly, SameSite=Strict cookie. WebSocket upgrades require that cookie.

Before internet exposure, add:

- TLS at a trusted reverse proxy;
- durable user identities and revocation;
- one OS/container boundary per workspace;
- origin validation and request-rate limiting;
- audit events without terminal contents or secrets;
- explicit filesystem and command policies for automation.

The future WebRTC media plane should remain separate from this control plane.
Short-lived, session-scoped credentials should connect the two.

