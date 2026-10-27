"""Run a benign console command against a temporary copy of a stopped test server (Linux)."""
import argparse
import fcntl
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
    args=parser.parse_args()
    root=Path(__file__).resolve().parents[1]
    binary=root/'target/debug/examples/console_smoke'
    with (args.source_data/'servers'/args.server_id/'process.lock').open('r+b') as lock:
        fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        with tempfile.TemporaryDirectory(prefix='blockops-console-smoke-') as directory:
            copy=Path(directory)/'data'
            shutil.copytree(args.source_data,copy,symlinks=True)
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
