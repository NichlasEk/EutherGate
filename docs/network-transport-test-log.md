# EutherGate network transport test log

Use this log to record which advertised WebRTC/TURN transport works on each
network. A successful test means both desktop video and DataChannel input work
for at least two minutes. Do not record TURN credentials, private candidate IP
addresses or company network details here.

## Test procedure

1. Open **DESKTOP** and start or select the remote output.
2. Select one profile from the protocol dropdown.
3. Wait for `LIVE`, then verify video, pointer and one harmless key press.
4. Copy the sanitized `ICE ROUTE` value from the footer into the table.
5. Mark the result `works`, `blocked`, `unstable` or `not tested`.

On a restrictive network, try `WORK · TURN/TLS 443` first. Try the UDP routes
for lower latency where UDP is permitted. `AUTO` remains the normal default.
Stop testing if the network explicitly reports that the destination or traffic
is prohibited.

## Results

| Date | Network label | Profile | Result | Sanitized ICE route | Notes |
| --- | --- | --- | --- | --- | --- |
| 2026-07-15 | Server-side baseline | WORK · TURN/TLS 443 | listener ready; client not tested | — | Caddy and TCP 443 listener active on relay host |
| 2026-07-15 | Server-side baseline | TURN/UDP 443 | listener ready; client not tested | — | TURN UDP 443 service active on relay host |
| 2026-07-15 | Server-side baseline | TURN/TCP 3478 | listener ready; client not tested | — | TURN TCP 3478 listener active on relay host |
| 2026-07-15 | Server-side baseline | TURN/UDP 3478 | listener ready; client not tested | — | TURN UDP 3478 listener active on relay host |
| 2026-07-15 | Local gateway | WORK · HTTPS/WSS | works | authenticated WSS | Complete 143312-byte JPEG frame received; input socket writable |
| 2026-07-15 | Local gateway | WORK · VNC/WSS | works | authenticated VNC/WSS | Complete 1280x720 RFB framebuffer received; input socket writable; private sockets removed after disconnect |
| YYYY-MM-DD | Work | WORK · VNC/WSS | not tested | authenticated VNC/WSS | — |
| YYYY-MM-DD | Work | WORK · HTTPS/WSS | not tested | — | — |
| YYYY-MM-DD | Work | WORK · TURN/TLS 443 | not tested | — | — |
| YYYY-MM-DD | Work | TURN/UDP 443 | not tested | — | — |
| YYYY-MM-DD | Mobile | AUTO | not tested | — | — |
| YYYY-MM-DD | Home | AUTO | not tested | — | — |
