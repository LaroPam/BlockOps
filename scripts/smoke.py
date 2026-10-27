"""Exercise two actual CLI processes and one relay process against a local TCP echo."""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading

root = Path(__file__).resolve().parents[1]
suffix = '.exe' if os.name == 'nt' else ''
relay_bin = root / 'target' / 'debug' / ('blockops-relay' + suffix)
client_bin = root / 'target' / 'debug' / ('blockops-tunnel' + suffix)
processes = []
def launch(args):
    child = subprocess.Popen([str(x) for x in args], stdout=subprocess.PIPE,
                             stderr=subprocess.DEVNULL, text=True)
    processes.append(child)
    return child

def line(child):
    # A watchdog below also bounds blocking pipe reads.
    value = child.stdout.readline().strip()
    if not value:
        raise RuntimeError('Process stopped before readiness')
    return value

def cleanup():
    for child in processes:
        if child.poll() is None:
            child.kill()

watchdog = threading.Timer(40, cleanup)
watchdog.start()
try:
    with socket.socket() as echo, tempfile.TemporaryDirectory() as directory:
        echo.bind(('127.0.0.1', 0))
        echo.listen()
        def serve():
            conn, _ = echo.accept()
            with conn:
                while data := conn.recv(8192):
                    conn.sendall(data)
        worker = threading.Thread(target=serve, daemon=True)
        worker.start()
        relay = launch([relay_bin, '--listen', '/ip4/127.0.0.1/tcp/0'])
        relay_address = line(relay)
        host = launch([client_bin, 'host', '--target', f'127.0.0.1:{echo.getsockname()[1]}', '--relay', relay_address])
        invitation = json.loads(line(host))
        path = Path(directory) / 'invite.json'
        path.write_text(json.dumps(invitation))
        guest = launch([client_bin, 'join', '--invite-file', path, '--bind', '127.0.0.1:0'])
        endpoint = line(guest).split()[2]
        ip, port = endpoint.rsplit(':', 1)
        with socket.create_connection((ip, int(port)), timeout=20) as client:
            payload = bytes(range(256)) * 100
            client.sendall(payload)
            client.shutdown(socket.SHUT_WR)
            received = bytearray()
            while data := client.recv(8192):
                received.extend(data)
            assert received == payload, 'Payload mismatch'
        worker.join(timeout=5)
        print('PASS: two CLI clients + separate relay; 25600 bytes and TCP half-close')
finally:
    watchdog.cancel()
    cleanup()
    for child in processes:
        child.wait(timeout=5)
