#!/usr/bin/env python3
"""Exercise HTTPS/WSS viewer exclusion, release and stale-viewer timeout."""

from __future__ import annotations

import json
import os
import time
import urllib.parse
import urllib.request

import websockets.sync.client


HTTP_BASE = os.environ.get("EUTHERGATE_SMOKE_URL", "http://127.0.0.1:8787")
TOKEN = os.environ.get("EUTHERGATE_TOKEN", "")
OUTPUT = os.environ.get("EUTHERGATE_SMOKE_OUTPUT", "forge:HEADLESS-1")


def request(path: str, cookie: str = "", method: str = "GET") -> tuple[int, dict]:
    headers = {"cookie": cookie} if cookie else {}
    value = urllib.request.Request(f"{HTTP_BASE}{path}", headers=headers, method=method)
    with urllib.request.urlopen(value, timeout=8) as response:
        body = response.read()
        return response.status, json.loads(body) if body else {}


def login() -> str:
    if not TOKEN:
        raise SystemExit("Set EUTHERGATE_TOKEN to the running gateway token")
    value = urllib.request.Request(
        f"{HTTP_BASE}/api/login",
        data=json.dumps({"token": TOKEN}).encode(),
        headers={"content-type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(value, timeout=5) as response:
        return response.headers["set-cookie"].split(";", 1)[0]


def viewer(cookie: str):
    ws_base = HTTP_BASE.replace("http://", "ws://").replace("https://", "wss://")
    return websockets.sync.client.connect(
        f"{ws_base}/ws/desktop-fallback",
        additional_headers={"Cookie": cookie},
        open_timeout=5,
        ping_interval=None,
    )


def wait_for_frame(socket, acknowledge: bool = True) -> None:
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        payload = socket.recv(timeout=max(0.1, deadline - time.monotonic()))
        if isinstance(payload, bytes) and payload.startswith(b"\xff\xd8"):
            if acknowledge:
                socket.send(json.dumps({"type": "frame_ack"}))
            return
    raise RuntimeError("viewer did not receive a JPEG frame")


def require_frame_gate(socket) -> None:
    wait_for_frame(socket, acknowledge=False)
    deadline = time.monotonic() + 0.6
    while time.monotonic() < deadline:
        try:
            payload = socket.recv(timeout=max(0.05, deadline - time.monotonic()))
        except TimeoutError:
            break
        if isinstance(payload, bytes) and payload.startswith(b"\xff\xd8"):
            raise RuntimeError("a second JPEG arrived before the first frame was acknowledged")
    socket.send(json.dumps({"type": "frame_ack"}))
    wait_for_frame(socket)


def wait_for_release(cookie: str, timeout: float = 5) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        _, status = request("/api/desktop/status", cookie)
        if not status["viewer_connected"]:
            return
        time.sleep(0.05)
    raise RuntimeError("viewer lease did not release")


def main() -> int:
    cookie = login()
    output = urllib.parse.quote(OUTPUT, safe="")
    request(f"/api/desktop/start?output={output}", cookie, "POST")

    first = viewer(cookie)
    wait_for_frame(first)
    first.send(json.dumps({"type": "heartbeat"}))
    try:
        second = viewer(cookie)
    except Exception:
        second = None
    if second is not None:
        second.close()
        first.close()
        raise RuntimeError("a second desktop viewer was accepted")
    first.close()
    wait_for_release(cookie)

    replacement = viewer(cookie)
    wait_for_frame(replacement)
    replacement.send(json.dumps({"type": "heartbeat"}))
    replacement.close()
    wait_for_release(cookie)

    gated = viewer(cookie)
    require_frame_gate(gated)
    gated.close()
    wait_for_release(cookie)

    healthy = viewer(cookie)
    wait_for_frame(healthy)
    healthy_until = time.monotonic() + 24
    next_heartbeat = time.monotonic() + 4
    while time.monotonic() < healthy_until:
        payload = healthy.recv(timeout=1)
        if isinstance(payload, bytes) and payload.startswith(b"\xff\xd8"):
            healthy.send(json.dumps({"type": "frame_ack"}))
        if time.monotonic() >= next_heartbeat:
            healthy.send(json.dumps({"type": "heartbeat"}))
            next_heartbeat += 4
    _, healthy_status = request("/api/desktop/status", cookie)
    if not healthy_status["viewer_connected"]:
        healthy.close()
        raise RuntimeError("a healthy heartbeat viewer lost its lease")
    healthy.close()
    wait_for_release(cookie)

    stale = viewer(cookie)
    wait_for_frame(stale)
    time.sleep(22)
    wait_for_release(cookie, timeout=4)
    stale.close()

    print("ok: viewer exclusion, frame gate, clean replacement and stale-viewer timeout passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
