import json
import socket
import sys

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.settimeout(10)
sock.connect(sys.argv[1])
stream = sock.makefile("rwb")

def receive():
    while True:
        message = json.loads(stream.readline())
        if "event" not in message:
            return message

def send(command, arguments=None):
    message = {"execute": command}
    if arguments is not None:
        message["arguments"] = arguments
    stream.write((json.dumps(message) + "\n").encode())
    stream.flush()
    result = receive()
    if "error" in result:
        raise RuntimeError(result)

if "QMP" not in receive():
    raise RuntimeError("invalid QMP greeting")
send("qmp_capabilities")
if sys.argv[2] == "input":
    events = [{"type": "key", "data": {"key": {"type": "qcode", "data": key}, "down": down}}
              for key in ["l", "a", "p", "u", "t", "a", "ret"] for down in [True, False]]
    send("input-send-event", {"events": events})
elif sys.argv[2] == "screenshot":
    send("screendump", {"filename": sys.argv[3], "format": "ppm"})
elif sys.argv[2] == "quit":
    send("quit")
else:
    raise ValueError(sys.argv[2])
