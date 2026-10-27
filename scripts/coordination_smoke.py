"""Four-process short-code smoke: coordinator, relay, host and guest.
Use --minecraft-port with an already running, explicitly accepted Minecraft 1.21.1.
Without it the target is an isolated local echo fixture.
"""
import argparse
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--minecraft-port', type=int)
    parser.add_argument('--minecraft-version', choices=['1.21.1', '1.21.4'], default='1.21.1')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    suffix = '.exe' if os.name == 'nt' else ''
    processes = []
    echo = None
    def launch(binary, *arguments):
        child = subprocess.Popen([str(root / 'target/debug' / (binary + suffix)), *arguments],
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        processes.append(child)
        return child
    def ready(child):
        result = child.stdout.readline().strip()
        if not result:
            reason = child.stderr.read().strip() if child.poll() is not None else ''
            raise RuntimeError(f'{Path(child.args[0]).name} failed before readiness: {reason[-1200:]}')
        return result
    def cleanup():
        for child in processes:
            if child.poll() is None:
                child.kill()
    watchdog = threading.Timer(50, cleanup)
    watchdog.start()
    try:
        port = args.minecraft_port
        if port is None:
            echo = socket.socket()
            echo.bind(('127.0.0.1', 0))
            echo.listen()
            port = echo.getsockname()[1]
            def serve_echo():
                stream, _ = echo.accept()
                with stream:
                    while data := stream.recv(8192):
                        stream.sendall(data)
            threading.Thread(target=serve_echo, daemon=True).start()
        coordinator = launch('blockops-coordination', 'serve', '--bind', '127.0.0.1:0')
        address = 'http://' + ready(coordinator).split()[1]
        relay = launch('blockops-relay', '--listen', '/ip4/127.0.0.1/tcp/0')
        relay_address = ready(relay)
        host = launch('blockops-coordination', 'host', '--coordinator', address,
                      '--target', f'127.0.0.1:{port}', '--relay', relay_address)
        code = ready(host)
        assert len(code) == 14, 'Unexpected short-code format'
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'code.txt'
            path.write_text(code)
            guest = launch('blockops-coordination', 'join', '--coordinator', address,
                           '--code-file', str(path), '--bind', '127.0.0.1:0')
            endpoint = ready(guest).split()[2]
            guest_port = int(endpoint.rsplit(':', 1)[1])
            if args.minecraft_port:
                from minecraft_smoke import status
                protocol = {'1.21.1': 767, '1.21.4': 769}[args.minecraft_version]
                assert status(guest_port, args.minecraft_version, protocol) == args.minecraft_version
            else:
                with socket.create_connection(('127.0.0.1', guest_port), timeout=15) as stream:
                    payload = bytes(range(256)) * 64
                    stream.sendall(payload)
                    stream.shutdown(socket.SHUT_WR)
                    result = bytearray()
                    while data := stream.recv(8192):
                        result.extend(data)
                    assert result == payload
            replay = launch('blockops-coordination', 'join', '--coordinator', address,
                            '--code-file', str(path), '--bind', '127.0.0.1:0')
            assert replay.wait(timeout=15) != 0, 'Code was reused'
        print('PASS: short code -> identity binding -> relay -> ' +
              ('Minecraft status/ping' if args.minecraft_port else 'binary TCP echo') + '; replay rejected')
    finally:
        watchdog.cancel()
        cleanup()
        for child in processes:
            child.wait(timeout=5)
        if echo:
            echo.close()


if __name__ == '__main__':
    main()
