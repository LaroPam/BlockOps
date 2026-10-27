"""Run a benign console command against a temporary copy of a stopped test server (Linux)."""
import argparse
import fcntl
import json
from pathlib import Path
import os
import shutil
import signal
import subprocess
import tempfile


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--source-data',required=True,type=Path)
    parser.add_argument('--server-id',required=True)
    parser.add_argument('--whitelist',action='store_true')
    args=parser.parse_args()
    root=Path(__file__).resolve().parents[1]
    binary=root/'target/debug/examples'/('whitelist_smoke' if args.whitelist else 'console_smoke')
    with (args.source_data/'servers'/args.server_id/'process.lock').open('r+b') as lock:
        fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        with tempfile.TemporaryDirectory(prefix='blockops-console-smoke-') as directory:
            copy=Path(directory)/'data'
            shutil.copytree(args.source_data,copy,symlinks=True)
            if args.whitelist:
                # A fixture exists only in this disposable copy; no real account is needed.
                cache = copy / 'servers' / args.server_id / 'usercache.json'
                cache.write_text(json.dumps([{'name': 'BlockOpsTest',
                    'uuid': '8bcb7d70-1f9f-48e3-b6ef-6f6d0fc55ee3',
                    'expiresOn': '2038-01-01 00:00:00 +0000'}]))
            child=subprocess.Popen([str(binary),str(copy),args.server_id],stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE,text=True,start_new_session=True)
            try:
                stdout,stderr=child.communicate(timeout=180)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid,signal.SIGKILL)
                child.communicate()
                raise RuntimeError('Console smoke timed out')
            if child.returncode:
                raise RuntimeError(f'Console smoke failed: {stderr}')
            print(stdout.strip())


if __name__=='__main__':
    main()
