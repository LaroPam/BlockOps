"""Verify an already-running Minecraft 1.21.1 server directly and through a local relay.
This script never installs Minecraft, accepts its EULA, or starts the Java server.
"""
import argparse
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import threading


def varint(value):
    result = bytearray()
    while True:
        byte = value & 127
        value >>= 7
        result.append(byte | (128 if value else 0))
        if not value:
            return bytes(result)


def exact(stream, length):
    result = bytearray()
    while len(result) < length:
        chunk = stream.recv(length - len(result))
        if not chunk:
            raise RuntimeError('Minecraft connection closed unexpectedly')
        result.extend(chunk)
    return bytes(result)


def read_varint(stream):
    value = 0
    for i in range(5):
        byte = exact(stream, 1)[0]
        value |= (byte & 127) << (7 * i)
        if not byte & 128:
            return value
    raise RuntimeError('Malformed VarInt')


def packet(stream, data):
    stream.sendall(varint(len(data)) + data)


def status(port, expected_version="1.21.1", protocol=767):
    with socket.create_connection(('127.0.0.1', port), timeout=20) as stream:
        host = b'localhost'
        packet(stream, b'\0' + varint(protocol) + varint(len(host)) + host + struct.pack('>H', port) + b'\1')
        packet(stream, b'\0')
        length = read_varint(stream)
        if not 0 < length < 1024 * 1024:
            raise RuntimeError('Invalid status packet size')
        import io
        payload = io.BytesIO(exact(stream, length))
        class Reader:
            def recv(self, n):
                return payload.read(n)
        reader = Reader()
        assert read_varint(reader) == 0, 'Unexpected status packet ID'
        data = json.loads(exact(reader, read_varint(reader)))
        assert not payload.read(), 'Trailing status bytes'
        assert data['version']['name'] == expected_version, data['version']
        assert data['version']['protocol'] == protocol, data['version']
        nonce = struct.pack('>q', 1234567890)
        packet(stream, b'\1' + nonce)
        assert read_varint(stream) == 9
        assert exact(stream, 9) == b'\1' + nonce, 'Minecraft ping/pong mismatch'
        return data['version']['name']


def main():
    args = argparse.ArgumentParser()
    args.add_argument('--port', type=int, default=25565)
    port = args.parse_args().port
    root = Path(__file__).resolve().parents[1]
    suffix = '.exe' if os.name == 'nt' else ''
    processes = []
    def launch(binary, *arguments):
        child = subprocess.Popen([str(root / 'target/debug' / (binary + suffix)), *arguments],
                                 stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        processes.append(child)
        return child
    def line(child):
        value = child.stdout.readline().strip()
        if not value:
            raise RuntimeError('Tunnel process failed before readiness')
        return value
    def cleanup():
        for child in processes:
            if child.poll() is None:
                child.kill()
    watchdog = threading.Timer(50, cleanup)
    watchdog.start()
    try:
        assert status(port) == '1.21.1'
        with tempfile.TemporaryDirectory() as directory:
            relay = launch('blockops-relay', '--listen', '/ip4/127.0.0.1/tcp/0')
            relay_address = line(relay)
            host = launch('blockops-tunnel', 'host', '--target', f'127.0.0.1:{port}', '--relay', relay_address)
            invite = json.loads(line(host))
            assert '/p2p-circuit/' in invite['address']
            path = Path(directory) / 'invite.json'
            path.write_text(json.dumps(invite))
            guest = launch('blockops-tunnel', 'join', '--invite-file', str(path), '--bind', '127.0.0.1:0')
            endpoint = line(guest).split()[2]
            guest_port = int(endpoint.rsplit(':', 1)[1])
            assert status(guest_port) == '1.21.1'
        print('PASS: actual Minecraft 1.21.1 status + ping/pong, directly and through host → relay → guest')
    finally:
        watchdog.cancel()
        cleanup()
        for child in processes:
            child.wait(timeout=5)


if __name__ == '__main__':
    main()
