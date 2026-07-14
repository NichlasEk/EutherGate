#!/usr/bin/env python3
"""Negotiate EutherGate WebRTC locally and require one decoded desktop frame."""

import json
import os
import threading
import time
import urllib.request
import urllib.parse

import gi
import websockets.sync.client

gi.require_version("Gst", "1.0")
gi.require_version("GstSdp", "1.0")
gi.require_version("GstWebRTC", "1.0")
from gi.repository import GLib, Gst, GstSdp, GstWebRTC  # noqa: E402


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


def main() -> int:
    cookie = login()
    start_desktop(cookie)
    ws_url = HTTP_BASE.replace("http://", "ws://").replace("https://", "wss://") + "/ws/desktop"
    socket = websockets.sync.client.connect(ws_url, additional_headers={"Cookie": cookie})

    Gst.init(None)
    loop = GLib.MainLoop()
    pipeline = Gst.Pipeline.new("euthergate-smoke")
    peer = Gst.ElementFactory.make("webrtcbin", "peer")
    peer.set_property("bundle-policy", GstWebRTC.WebRTCBundlePolicy.MAX_BUNDLE)
    pipeline.add(peer)
    received_frame = threading.Event()
    failed: list[str] = []

    def send(message: dict) -> None:
        socket.send(json.dumps(message, separators=(",", ":")))

    def on_ice(_peer, mline: int, candidate: str) -> None:
        send({"type": "ice", "candidate": candidate, "sdpMLineIndex": mline})

    def on_offer_created(promise, element, _unused) -> None:
        promise.wait()
        reply = promise.get_reply()
        if reply is None or not reply.has_field("offer"):
            failed.append("WebRTC receiver could not create an SDP offer")
            return
        offer = reply.get_value("offer")
        element.emit("set-local-description", offer, Gst.Promise.new())
        send({"type": "offer", "sdp": offer.sdp.as_text()})

    def on_pad_added(_peer, pad) -> None:
        receiver = Gst.parse_bin_from_description(
            "rtpvp8depay ! vp8dec ! fakesink name=sink sync=false signal-handoffs=true",
            True,
        )
        pipeline.add(receiver)
        receiver.get_by_name("sink").connect("handoff", lambda *_: received_frame.set())
        receiver.sync_state_with_parent()
        pad.link(receiver.get_static_pad("sink"))

    def signaling_reader() -> None:
        try:
            for raw in socket:
                message = json.loads(raw)
                if message.get("type") == "answer":
                    GLib.idle_add(set_answer, message["sdp"])
                elif message.get("type") == "ice":
                    GLib.idle_add(
                        peer.emit,
                        "add-ice-candidate",
                        int(message.get("sdpMLineIndex", 0)),
                        message["candidate"],
                    )
                elif message.get("type") in {"error", "fatal"}:
                    failed.append(message.get("message", "media helper failed"))
        except Exception as error:
            if not received_frame.is_set():
                failed.append(str(error))

    def set_answer(sdp_text: str) -> bool:
        result, sdp = GstSdp.SDPMessage.new()
        if result != GstSdp.SDPResult.OK:
            failed.append("could not allocate remote SDP")
            return False
        if GstSdp.sdp_message_parse_buffer(sdp_text.encode(), sdp) != GstSdp.SDPResult.OK:
            failed.append("gateway returned invalid SDP")
            return False
        answer = GstWebRTC.WebRTCSessionDescription.new(GstWebRTC.WebRTCSDPType.ANSWER, sdp)
        peer.emit("set-remote-description", answer, Gst.Promise.new())
        return False

    peer.connect("on-ice-candidate", on_ice)
    peer.connect("pad-added", on_pad_added)
    caps = Gst.Caps.from_string(
        "application/x-rtp,media=video,encoding-name=VP8,payload=120,clock-rate=90000"
    )
    peer.emit("add-transceiver", GstWebRTC.WebRTCRTPTransceiverDirection.RECVONLY, caps)
    pipeline.set_state(Gst.State.PLAYING)
    channel = peer.emit("create-data-channel", "input", None)
    if channel is None:
        raise RuntimeError("receiver could not create WebRTC input DataChannel")
    channel.connect(
        "on-open",
        lambda *_: channel.emit(
            "send-string",
            json.dumps({"type": "pointer_move", "x": 100, "y": 100}),
        ),
    )

    threading.Thread(target=signaling_reader, name="smoke-signaling", daemon=True).start()
    promise = Gst.Promise.new_with_change_func(on_offer_created, peer, None)
    peer.emit("create-offer", None, promise)

    started = time.monotonic()

    def check_result() -> bool:
        if received_frame.is_set() or failed or time.monotonic() - started > 15:
            loop.quit()
            return False
        return True

    GLib.timeout_add(100, check_result)
    loop.run()
    pipeline.set_state(Gst.State.NULL)
    socket.close()
    if failed:
        raise RuntimeError(failed[0])
    if not received_frame.is_set():
        raise TimeoutError("no decoded WebRTC desktop frame arrived within 15 seconds")
    print("ok: WebRTC negotiated, VP8 desktop frame decoded, DataChannel opened")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
