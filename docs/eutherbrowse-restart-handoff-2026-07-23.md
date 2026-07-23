# EutherBrowse live restart handoff — 2026-07-23

## Slice being deployed

The first EutherBrowse slice adds authenticated home Firefox windows to the
Forge terminal UI:

- `OPEN BROWSER` launches ChatGPT in the persistent Forge Sway compositor.
- Every Firefox top-level window receives its own workspace and session chip.
- Selecting a chip focuses that exact Firefox window.
- The browser view reuses the existing Forge HTTPS/WSS image and input bridge.
- Firefox login state remains in the private home profile and is never copied
  into EutherGate.

The pre-deploy repository base is `045a312`.

## Files in the slice

- `src/main.rs`
- `web/src/main.ts`
- `web/src/style.css`
- `docs/eutherbrowse-first-slice.md`
- this handoff

## Verification completed before restart

- `cargo test`: 19 passed.
- `cargo build --release --bin euthergate`: passed.
- `npm run build` in `web/`: passed.
- `git diff --check`: passed.
- Isolated API smoke opened two Firefox windows on workspaces 10 and 11.
- The first window navigated to ChatGPT.
- `scripts/smoke_wss_desktop.py` received a real Forge frame and writable input.
- A captured 1280×720 frame was visually checked and showed ChatGPT.
- A live smoke exposed a globally sideloaded Video DownloadHelper welcome tab;
  the dedicated profile now limits enabled extensions to its own profile scope.
- The corrected live launch opened ChatGPT on workspace 10, returned a JPEG
  frame through HTTPS/WSS and accepted pointer input. That window was left open
  for the user.

## Live rollout

Only `euthergate.service` needs a restart. `euthergate-forge.service`, tmux and
the tunnel remain running. Schedule the restart out of process:

```sh
systemd-run --user --collect --on-active=2s \
  --unit=euthergate-browser-deploy-restart \
  /usr/bin/systemctl --user restart euthergate.service
```

Afterward verify:

```sh
systemctl --user is-active euthergate.service
curl --fail --silent http://127.0.0.1:8787/api/health
```

An authenticated `GET /api/browser/sessions` should return an empty session
list until the user clicks `OPEN BROWSER`. The first launch creates or reuses
`~/.local/share/euthergate/browser/firefox-profile`.

## Rollback

Return to the previous Git revision, rebuild the release binary and web bundle,
then use the same delayed service restart. No Forge restart is required. The
Firefox profile may remain in place; it is not read by the previous revision.
