# EutherBrowse SWGL live restart handoff — 2026-07-23

## Problem and measured cause

New ChatGPT replies completed and exposed their copy/feedback/source controls,
but their streamed Markdown remained visually blank. Old conversations loaded
from navigation were visible.

Firefox 152.0.6 `about:support` on Forge reported:

- hardware compositing blocked by the platform;
- ordinary WebRender blocklisted by `gfxInfo`;
- the virtual display as `1280x720@30Hz`.

An isolated cookie-free Firefox profile with `gfx.webrender.all=true`,
`gfx.webrender.software=true` and `MOZ_WEBRENDER=1` reported
`Compositing: WebRender (Software)`.

## Slice being deployed

- The private EutherBrowse profile enables CPU-backed software WebRender.
- Firefox launches with both native Wayland and `MOZ_WEBRENDER=1`.
- No cookies, profile contents or login data are added to the repository.

The pre-change repository base is `e240fde`.

## Verification completed before restart

- `cargo fmt --check`: passed.
- `cargo test`: 19 passed.
- `cargo build --release --bin euthergate`: passed.
- `npm run build`: passed.
- Python helper compilation and `git diff --check`: passed.
- Isolated runtime probe: `Compositing: WebRender (Software)`.

## Live transition

Only `euthergate.service` and the dedicated EutherBrowse Firefox process need
to restart. Do not restart `euthergate-forge.service`, tmux or the reverse
tunnel.

1. Schedule `euthergate.service` restart out of process.
2. Verify `systemctl --user is-active euthergate.service` and `/api/health`.
3. Close the single Firefox container on Forge workspace 10.
4. Relaunch the private profile on workspace 10 with:

   ```text
   MOZ_ENABLE_WAYLAND=1 MOZ_WEBRENDER=1 firefox --new-instance \
     --profile ~/.local/share/euthergate/browser/firefox-profile \
     --new-window https://chatgpt.com/
   ```

5. Verify the new Firefox container is visible to the browser-session API and
   that an `about:support` probe reports software WebRender.

The persistent private profile keeps the OpenAI login and cookies across this
browser restart.

## Rollback

Return to `e240fde`, rebuild the release binary, restart only
`euthergate.service`, remove the two `gfx.webrender` preferences from the
private profile's generated `user.js`, and restart only the EutherBrowse
Firefox process.
