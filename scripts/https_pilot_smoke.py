"""Exercise an HTTPS coordinator with verified TLS and a real relay.

Requires openssl and the debug binaries. Uses a private, temporary test CA and a local
TLS terminator; never changes the system trust store or uses an insecure TLS mode.
"""
import asyncio
import os
from pathlib import Path
import socket
import ssl
import subprocess
import tempfile
import threading

ROOT = Path(__file__).resolve().parents[1]
SUFFIX = '.exe' if os.name == 'nt' else ''


def openssl(*args):
    subprocess.run(['openssl', *map(str, args)], check=True, stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL)


class Proxy:
    def __init__(self, context, destination):
        self.loop = asyncio.new_event_loop()
        self.context = context
        self.destination = destination
        self.ready = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()
        if not self.ready.wait(10):
            raise RuntimeError('TLS proxy did not start')
        if hasattr(self, 'error'):
            raise self.error

    async def handle(self, client_reader, client_writer):
        try:
            upstream_reader, upstream_writer = await asyncio.open_connection(*self.destination)
            async def pump(reader, writer):
                try:
                    while data := await reader.read(65536):
                        writer.write(data)
                        await writer.drain()
                except (ConnectionError, OSError):
                    pass
                finally:
                    writer.close()
            await asyncio.gather(pump(client_reader, upstream_writer),
                                 pump(upstream_reader, client_writer))
        except (ConnectionError, OSError):
            client_writer.close()

    def run(self):
        asyncio.set_event_loop(self.loop)
        try:
            self.server = self.loop.run_until_complete(asyncio.start_server(
                self.handle, '127.0.0.1', 0, ssl=self.context))
            self.port = self.server.sockets[0].getsockname()[1]
        except Exception as error:
            self.error = error
            self.ready.set()
            return
        self.ready.set()
        self.loop.run_forever()
        self.server.close()
        self.loop.run_until_complete(self.server.wait_closed())
        tasks = asyncio.all_tasks(self.loop)
        for task in tasks:
            task.cancel()
        self.loop.run_until_complete(asyncio.gather(*tasks, return_exceptions=True))
        self.loop.close()

    def close(self):
        self.loop.call_soon_threadsafe(self.loop.stop)
        self.thread.join(timeout=5)


def main():
    children = []
    proxy = None
    echo = None
    def launch(binary, *args, env=None):
        child = subprocess.Popen([str(ROOT / 'target/debug' / (binary + SUFFIX)), *args],
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)
        children.append(child)
        return child
    def line(child):
        text = child.stdout.readline().strip()
        if not text:
            raise RuntimeError('A process failed before readiness')
        return text
    def cleanup():
        for child in children:
            if child.poll() is None:
                child.kill()
    watchdog = threading.Timer(50, cleanup)
    watchdog.start()
    try:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            ca_key, ca_cert = path / 'ca.key', path / 'ca.pem'
            wrong_key, wrong_cert = path / 'wrong.key', path / 'wrong.pem'
            server_key, server_csr, server_cert = path / 'server.key', path / 'server.csr', path / 'server.pem'
            ext = path / 'ext.cnf'
            ext.write_text('subjectAltName=IP:127.0.0.1\nextendedKeyUsage=serverAuth\n')
            for key, cert, name in [(ca_key, ca_cert, 'BlockOps test CA'),
                                    (wrong_key, wrong_cert, 'Untrusted test CA')]:
                openssl('req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', key,
                        '-out', cert, '-days', '1', '-subj', '/CN=' + name,
                        '-addext', 'basicConstraints=critical,CA:TRUE')
            openssl('req', '-newkey', 'rsa:2048', '-nodes', '-keyout', server_key,
                    '-out', server_csr, '-subj', '/CN=127.0.0.1')
            openssl('x509', '-req', '-in', server_csr, '-CA', ca_cert, '-CAkey', ca_key,
                    '-CAcreateserial', '-out', server_cert, '-days', '1', '-extfile', ext)
            coordinator = launch('blockops-coordination', 'serve', '--bind', '127.0.0.1:0')
            backend_port = int(line(coordinator).split()[1].rsplit(':', 1)[1])
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.load_cert_chain(server_cert, server_key)
            proxy = Proxy(context, ('127.0.0.1', backend_port))
            origin = f'https://127.0.0.1:{proxy.port}'
            relay = launch('blockops-relay', '--listen', '/ip4/127.0.0.1/tcp/0')
            relay_address = line(relay)
            echo = socket.socket()
            echo.bind(('127.0.0.1', 0))
            echo.listen()
            def serve_echo():
                stream, _ = echo.accept()
                with stream:
                    while data := stream.recv(8192):
                        stream.sendall(data)
            threading.Thread(target=serve_echo, daemon=True).start()
            target = f'127.0.0.1:{echo.getsockname()[1]}'
            # Reject a server whose CA was not trusted, without suppressing certificate checks.
            untrusted = dict(os.environ, SSL_CERT_FILE=str(wrong_cert))
            bad = launch('blockops-coordination', 'host', '--coordinator', origin,
                         '--target', target, '--relay', relay_address, env=untrusted)
            _, err = bad.communicate(timeout=20)
            if bad.returncode == 0 or 'certificate' not in err.lower():
                raise RuntimeError('Untrusted CA was not rejected as a certificate error')
            trusted = dict(os.environ, SSL_CERT_FILE=str(ca_cert))
            host = launch('blockops-coordination', 'host', '--coordinator', origin,
                          '--target', target, '--relay', relay_address, env=trusted)
            code_path = path / 'code.txt'
            code_path.write_text(line(host))
            guest = launch('blockops-coordination', 'join', '--coordinator', origin,
                           '--code-file', str(code_path), '--bind', '127.0.0.1:0', env=trusted)
            port = int(line(guest).split()[2].rsplit(':', 1)[1])
            with socket.create_connection(('127.0.0.1', port), timeout=15) as stream:
                payload = bytes(range(256)) * 64
                stream.sendall(payload)
                stream.shutdown(socket.SHUT_WR)
                received = bytearray()
                while data := stream.recv(8192):
                    received.extend(data)
                assert received == payload
            print('PASS: verified HTTPS coordinator + separate relay/clients; untrusted CA rejected')
    finally:
        watchdog.cancel()
        cleanup()
        for child in children:
            child.wait(timeout=5)
        if proxy:
            proxy.close()
        if echo:
            echo.close()


if __name__ == '__main__':
    main()
