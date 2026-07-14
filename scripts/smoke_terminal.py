#!/usr/bin/env python3
"""End-to-end smoke test for an already running EutherGate gateway."""

import asyncio
import json
import os
import urllib.error
import urllib.request

import websockets


HTTP_BASE = os.environ.get("EUTHERGATE_SMOKE_URL", "http://127.0.0.1:8787")
TOKEN = os.environ.get("EUTHERGATE_TOKEN", "")
MARKER = "EUTHERGATE_RECONNECT_OK"


def login() -> str:
    if not TOKEN:
        raise SystemExit("Set EUTHERGATE_TOKEN to the token used by the running gateway")
    request = urllib.request.Request(
        f"{HTTP_BASE}/api/login",
        data=json.dumps({"token": TOKEN}).encode(),
        headers={"content-type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=5) as response:
        return response.headers["set-cookie"].split(";", 1)[0]


async def receive_until(socket, needle: str, timeout: float = 5) -> bytes:
    output = bytearray()

    async def receive() -> bytes:
        while needle.encode() not in output:
            message = await socket.recv()
            output.extend(message.encode() if isinstance(message, str) else message)
        return bytes(output)

    return await asyncio.wait_for(receive(), timeout)


async def smoke() -> None:
    cookie = login()
    ws_url = HTTP_BASE.replace("http://", "ws://").replace("https://", "wss://")
    ws_url += "/ws/terminal"

    async with websockets.connect(ws_url, additional_headers={"Cookie": cookie}) as socket:
        await socket.send(f"printf '\\n{MARKER}\\n'\n".encode())
        await receive_until(socket, MARKER)

    async with websockets.connect(ws_url, additional_headers={"Cookie": cookie}) as socket:
        replay = await receive_until(socket, MARKER)
        if MARKER.encode() not in replay:
            raise RuntimeError("terminal marker was not replayed after reconnect")

    print("ok: authenticated PTY output survived WebSocket reconnect")


if __name__ == "__main__":
    asyncio.run(smoke())

