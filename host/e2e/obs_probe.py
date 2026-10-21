"""Read-only OBS query for the e2e checks: program scene, scenes, record and
stream state, and each input's mute. Speaks obs-websocket v5 over a raw
socket so the tests need nothing beyond the standard library."""
import base64, hashlib, json, os, socket, struct

def connect(port):
    s = socket.create_connection(("127.0.0.1", port))
    key = base64.b64encode(os.urandom(16)).decode()
    s.sendall(f"GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
              f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n"
              "Sec-WebSocket-Protocol: obswebsocket.json\r\n\r\n".encode())
    head = b""
    while b"\r\n\r\n" not in head:
        head += s.recv(1)
    assert b" 101 " in head, head
    return s

def send(s, obj):
    data = json.dumps(obj).encode()
    mask = os.urandom(4)
    size = bytes([0x80 | len(data)]) if len(data) < 126 else bytes([0x80 | 126]) + struct.pack(">H", len(data))
    s.sendall(bytes([0x81]) + size + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))

def recv(s):
    def n(k):
        b = b""
        while len(b) < k:
            b += s.recv(k - len(b))
        return b
    _, b1 = n(2)
    size = b1 & 0x7F
    if size == 126:
        size = struct.unpack(">H", n(2))[0]
    elif size == 127:
        size = struct.unpack(">Q", n(8))[0]
    return json.loads(n(size))

cfg = json.load(open(os.path.expanduser("~/.config/obs-studio/plugin_config/obs-websocket/config.json")))
s = connect(cfg["server_port"])
hello = recv(s)["d"]
identify = {"rpcVersion": 1, "eventSubscriptions": 0}
if "authentication" in hello:
    a = hello["authentication"]
    secret = base64.b64encode(hashlib.sha256((cfg["server_password"] + a["salt"]).encode()).digest()).decode()
    identify["authentication"] = base64.b64encode(hashlib.sha256((secret + a["challenge"]).encode()).digest()).decode()
send(s, {"op": 1, "d": identify})
assert recv(s)["op"] == 2

def request(kind, data=None):
    send(s, {"op": 6, "d": {"requestType": kind, "requestId": kind, "requestData": data or {}}})
    while True:
        m = recv(s)
        if m["op"] == 7 and m["d"]["requestId"] == kind:
            return m["d"].get("responseData", {})

print("program scene:", request("GetCurrentProgramScene").get("sceneName"))
print("scenes:", [x["sceneName"] for x in request("GetSceneList")["scenes"]])
print("record:", request("GetRecordStatus").get("outputActive"), "stream:", request("GetStreamStatus").get("outputActive"))
for i in request("GetInputList")["inputs"]:
    muted = request("GetInputMute", {"inputName": i["inputName"]}).get("inputMuted")
    print("input:", repr(i["inputName"]), "muted" if muted else ("live" if muted is False else "-"))
