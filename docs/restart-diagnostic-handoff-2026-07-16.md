# EutherGate restart and diagnostic handoff — 2026-07-16

## Request

Restart EutherGate, then inspect the post-restart logs and determine why the desktop path is misbehaving.

## Verified state before restart

- Repository: `/home/nichlas/EutherGate`, branch `main`, clean working tree before this handoff.
- `GET http://127.0.0.1:8787/api/health` returned `{"status":"ok"}`.
- `euthergate.service` was active since 2026-07-15 16:37:53 CEST, PID 1457290.
- `euthergate-forge.service` was active since 2026-07-15 13:35:07 CEST.
- `euthergate-tunnel.service` was active since 2026-07-15 13:35:06 CEST.

## Relevant pre-restart log evidence

- Repeated desktop capture stop/start cycles on physical output `HDMI-A-1`.
- At 08:13:03 and 08:13:05, virtual-desktop start attempts failed with HTTP 503:
  `disconnect the current viewer before switching output`.
- At 08:13:12, the desktop VNC WebSocket disconnected with:
  `Connection reset without closing handshake`.
- Capture later restarted on `HDMI-A-1` at 08:28:38.

## Restart procedure

Use a delayed user-systemd transient unit so the restart is not issued from inside the gateway's own process group:

```sh
systemd-run --user --unit=euthergate-delayed-restart-20260716 --on-active=3s \
  /usr/bin/systemctl --user restart euthergate.service
```

After reconnecting, verify:

```sh
systemctl --user status euthergate.service euthergate-forge.service euthergate-tunnel.service --no-pager
curl -fsS http://127.0.0.1:8787/api/health
journalctl --user -u euthergate.service --since '2026-07-16 08:30:00' --no-pager
```

The initial diagnostic hypothesis is a stale/simultaneous desktop viewer: an existing physical-output viewer remains registered while the client tries to switch to the Forge virtual output. Confirm from post-restart logs and API state before changing code.

## Verified result

- The delayed restart completed at 2026-07-16 08:31:10 CEST.
- `euthergate.service` restarted cleanly with new PID 3428519 and logged `EutherGate is ready` immediately.
- `/api/health` returned `{"status":"ok"}` after restart.
- Forge and tunnel remained active throughout.
- No new EutherGate warnings or errors appeared in the post-restart journal during the verification window.

## Diagnosis

The pre-restart failures match an output-switch race, not a gateway startup or tunnel failure:

1. The backend intentionally rejects an output change while `viewer_connected` is true (`src/main.rs`, `DesktopManager::start`).
2. The frontend closes the current desktop client and waits a fixed 250 ms before posting the new output selection (`web/src/main.ts`, `switchDesktopOutput`).
3. The observed VNC WebSocket did not finish disconnecting until several seconds after the rejected switch attempts. Therefore the backend still saw the old viewer and correctly returned HTTP 503.
4. Restarting the gateway resets the in-memory viewer flag and cleared the immediate stale state.

If the problem returns when switching outputs, the durable fix is to replace the fixed 250 ms delay with an explicit viewer-disconnected acknowledgement/status wait before requesting the new output. That change was not made during this diagnosis-only restart task.
