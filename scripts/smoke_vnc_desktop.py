#!/usr/bin/env python3
"""Require one real RFB frame and a writable input path over VNC/WSS."""

import json
import os
import struct
import time
import urllib.parse
import urllib.request

import websockets.sync.client


HTTP_BASE = os.environ.get("EUTHERGATE_SMOKE_URL", "http://127.0.0.1:8787")
TOKEN = os.environ.get("EUTHERGATE_TOKEN", "")
OUTPUT = os.environ.get("EUTHERGATE_SMOKE_OUTPUT", "")


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


class RfbStream:
    def __init__(self, socket) -> None:
        self.socket = socket
        self.buffer = bytearray()

    def read(self, length: int) -> bytes:
        while len(self.buffer) < length:
            payload = self.socket.recv(timeout=8)
            if not isinstance(payload, bytes):
                raise RuntimeError("VNC gateway returned a non-binary WebSocket message")
            self.buffer.extend(payload)
        result = bytes(self.buffer[:length])
        del self.buffer[:length]
        return result


def main() -> int:
    cookie = login()
    start_desktop(cookie)
    ws_url = HTTP_BASE.replace("http://", "ws://").replace("https://", "wss://")
    socket = websockets.sync.client.connect(
        f"{ws_url}/ws/desktop-vnc",
        additional_headers={"Cookie": cookie},
        open_timeout=5,
        max_size=16 * 1024 * 1024,
    )
    stream = RfbStream(socket)

    version = stream.read(12)
    if not version.startswith(b"RFB 003."):
        raise RuntimeError(f"invalid RFB version banner: {version!r}")
    socket.send(b"RFB 003.008\n")
    security_types = stream.read(stream.read(1)[0])
    if 1 not in security_types:
        raise RuntimeError(f"WayVNC did not offer private no-auth RFB: {security_types!r}")
    socket.send(b"\x01")
    if stream.read(4) != b"\x00\x00\x00\x00":
        raise RuntimeError("WayVNC rejected the private RFB connection")
    socket.send(b"\x01")

    server_init = stream.read(24)
    width, height = struct.unpack(">HH", server_init[:4])
    bytes_per_pixel = server_init[4] // 8
    name_length = struct.unpack(">I", server_init[20:24])[0]
    desktop_name = stream.read(name_length).decode("utf-8", errors="replace")
    if not width or not height or bytes_per_pixel not in {1, 2, 4}:
        raise RuntimeError("WayVNC returned invalid framebuffer geometry")

    socket.send(struct.pack(">BBHi", 2, 0, 1, 0))  # raw encoding only
    frame_bytes = 0
    first_pixel = None
    varied_pixels = False
    for attempt in range(3):
        socket.send(struct.pack(">BBHHHH", 3, 0, 0, 0, width, height))
        update = stream.read(4)
        if update[0] != 0:
            raise RuntimeError(f"expected framebuffer update, received RFB message {update[0]}")
        rectangles = struct.unpack(">H", update[2:4])[0]
        for _ in range(rectangles):
            rectangle = stream.read(12)
            _, _, rect_width, rect_height, encoding = struct.unpack(">HHHHi", rectangle)
            if encoding != 0:
                raise RuntimeError(f"unexpected RFB encoding {encoding}")
            payload_length = rect_width * rect_height * bytes_per_pixel
            payload = stream.read(payload_length)
            if not varied_pixels:
                for offset in range(0, len(payload), bytes_per_pixel):
                    pixel = payload[offset : offset + bytes_per_pixel]
                    if first_pixel is None:
                        first_pixel = pixel
                    elif pixel != first_pixel:
                        varied_pixels = True
                        break
            frame_bytes += payload_length
        if varied_pixels:
            break
        if attempt < 2:
            time.sleep(0.4)

    socket.send(struct.pack(">BBHH", 5, 0, min(100, width - 1), min(100, height - 1)))
    socket.close()
    if not frame_bytes:
        raise RuntimeError("WayVNC returned no framebuffer pixels")
    if not varied_pixels:
        raise RuntimeError("WayVNC returned a uniform framebuffer with no visible desktop content")
    print(
        f"ok: varied VNC/WSS RFB frame received ({width}x{height}, {frame_bytes} bytes), "
        f"input writable, desktop={desktop_name!r}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
