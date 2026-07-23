#!/usr/bin/env python3
"""Require one JPEG frame and a writable input path over desktop HTTPS/WSS."""

import json
import os
import time
import urllib.parse
import urllib.request

import websockets.sync.client


HTTP_BASE = os.environ.get("EUTHERGATE_SMOKE_URL", "http://127.0.0.1:8787")
TOKEN = os.environ.get("EUTHERGATE_TOKEN", "")
OUTPUT = os.environ.get("EUTHERGATE_SMOKE_OUTPUT", "")
CLICK = os.environ.get("EUTHERGATE_SMOKE_CLICK") == "1"
POINTER_X = int(os.environ.get("EUTHERGATE_SMOKE_X", "100"))
POINTER_Y = int(os.environ.get("EUTHERGATE_SMOKE_Y", "100"))
TEXT = os.environ.get("EUTHERGATE_SMOKE_TEXT", "")


def login() -> str:
    if not TOKEN:
        raise SystemExit("Set EUTHERGATE_TOKEN to the running gateway token")
    request = urllib.request.Request(
        f"{HTTP_BASE}/api/login",
        data=json.dumps({"token": TOKEN}).encode(),
        headers={"content-type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=5) as response:
        return response.headers["set-cookie"].split(";", 1)[0]


def start_desktop(cookie: str) -> None:
    query = f"?output={urllib.parse.quote(OUTPUT)}" if OUTPUT else ""
    request = urllib.request.Request(
        f"{HTTP_BASE}/api/desktop/start{query}",
        headers={"cookie": cookie},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=8) as response:
        if response.status != 200:
            raise RuntimeError("virtual desktop did not start")


def main() -> int:
    cookie = login()
    start_desktop(cookie)
    ws_url = HTTP_BASE.replace("http://", "ws://").replace("https://", "wss://")
    socket = websockets.sync.client.connect(
        f"{ws_url}/ws/desktop-fallback",
        additional_headers={"Cookie": cookie},
        open_timeout=5,
    )
    ready = False
    frame = b""
    for payload in socket:
        if isinstance(payload, str):
            message = json.loads(payload)
            if message.get("type") == "ready":
                ready = True
            elif message.get("type") in {"error", "fatal", "input-warning"}:
                raise RuntimeError(message.get("message", "fallback helper failed"))
        elif payload.startswith(b"\xff\xd8") and payload.endswith(b"\xff\xd9"):
            frame = payload
            socket.send(json.dumps({"type": "frame_ack"}))
            break
    socket.send(json.dumps({"type": "pointer_move", "x": POINTER_X, "y": POINTER_Y}))
    input_ack = not TEXT
    if TEXT:
        socket.send(json.dumps({"type": "text", "text": TEXT}))
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            payload = socket.recv(timeout=max(0.1, deadline - time.monotonic()))
            if isinstance(payload, bytes) and payload.startswith(b"\xff\xd8"):
                socket.send(json.dumps({"type": "frame_ack"}))
            elif isinstance(payload, str):
                message = json.loads(payload)
                if message.get("type") == "input-ack" and message.get("input") == "text":
                    input_ack = True
                    break
        if not input_ack:
            raise RuntimeError("HTTPS/WSS helper did not acknowledge text input")
    if CLICK:
        socket.send(json.dumps({"type": "pointer_button", "button": 0, "state": "pressed"}))
        time.sleep(0.05)
        socket.send(json.dumps({"type": "pointer_button", "button": 0, "state": "released"}))
    socket.send(json.dumps({"type": "release_control"}))
    socket.close()
    if not ready:
        raise RuntimeError("HTTPS/WSS helper did not announce readiness")
    if not frame:
        raise RuntimeError("no complete JPEG desktop frame arrived")
    if TEXT:
        input_result = "text input acknowledged"
    elif CLICK:
        input_result = "real pointer click sent"
    else:
        input_result = "input socket writable"
    print(f"ok: HTTPS/WSS JPEG frame received ({len(frame)} bytes), {input_result}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
