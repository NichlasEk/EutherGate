# EutherBrowse latency restart handoff — 2026-07-23

## Intended restart

Deploy only the EutherGate binary and static web build for the local-cursor and
latest-frame-wins JPEG slice. Restart `euthergate.service` with a delayed
transient user unit so the active request can finish.

Do not restart:

- `euthergate-forge.service` (pre-restart PID 1294)
- the persistent tmux server (pre-restart PID 1295)
- the reverse SSH tunnel (pre-restart PID 1914)

The old Gate PID before the restart is 2149681. The desktop is active on Forge
Session and no viewer is connected. The current user Firefox window is Sway
container 31 on workspace 10 and must survive the Gate restart.

## Expected result

- `/api/health` returns `{"status":"ok"}` from a new Gate PID.
- Forge, tmux and tunnel PIDs are unchanged.
- Firefox container 31 remains listed on workspace 10.
- WSS sends one JPEG, waits for `frame_ack`, and then sends the most recent
  pending JPEG.
- EutherBrowse capture omits the compositor cursor and relies on the immediate
  lime browser cursor.

## Recovery

If the new Gate fails, inspect `journalctl --user -u euthergate.service` and
restart only `euthergate.service`. Forge, tmux, the tunnel and Firefox are
separate persistent processes and should not be terminated as part of recovery.

