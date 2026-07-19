# Tmux terminal sessions

## Shipped design

EutherGate uses the dedicated `euthergate` tmux socket. The independent
`euthergate-tmux.service` owns that server, while `euthergate.service` owns only
the browser-facing PTY clients. Restarting the gateway therefore detaches its
clients without terminating shells or programs inside tmux.

The authenticated terminal UI can:

- list managed sessions;
- create a session with a strictly validated 1-32 character name;
- switch the WebSocket terminal between sessions;
- remember the selected session per browser;
- launch Kitty on the selected remote desktop, attached to the same session.

The default session is `gate`. Local inspection and attachment use:

```bash
tmux -L euthergate list-sessions
tmux -L euthergate attach-session -t gate
```

## Deployment boundary

The first migration cannot preserve the old gateway-owned shell because it was
not started inside tmux. Before restarting `euthergate.service`, finish and
publish work in that shell. Install and start `euthergate-tmux.service` first,
then restart only the gateway. Subsequent gateway restarts preserve all managed
sessions.

Do not routinely restart `euthergate-tmux.service`: doing so intentionally
stops the tmux server and its sessions. The installer uses `start`, not
`restart`, for this reason.

## Verified checks

- Rust unit tests and strict Clippy pass.
- TypeScript and Vite production build pass.
- Authenticated terminal WebSocket reconnect and replay pass.
- Session create/list API returns `gate` and an additional named session.
- An isolated gateway was stopped with SIGTERM while tmux ran separately;
  `gate` remained alive with `zsh` and accepted a new gateway attachment.
