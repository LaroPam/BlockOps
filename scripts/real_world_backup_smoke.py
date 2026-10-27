"""Test backup and restore on an isolated copy of an existing stopped Linux server.

Requires a previously installed server and EULA accepted for that server; does not
create or start a server. The original data directory stays locked and is never edited.
"""
import argparse
import fcntl
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile


def digest(path):
    with path.open('rb') as file:
        return hashlib.file_digest(file, 'sha256').hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--source-data', required=True, type=Path)
    parser.add_argument('--server-id', required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    executable = root / 'target/debug/blockops-server-manager'
    original_server = args.source_data / 'servers' / args.server_id
    lock_path = original_server / 'process.lock'
    # A lock file is created by the manager before a server is ever started.
    with lock_path.open('r+b') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        original_level = original_server / 'world' / 'level.dat'
        before = digest(original_level)
        with tempfile.TemporaryDirectory(prefix='blockops-backup-smoke-') as temporary:
            cloned = Path(temporary) / 'data'
            shutil.copytree(args.source_data, cloned, symlinks=True)
            server = cloned / 'servers' / args.server_id
            def command(*arguments):
                return subprocess.run([str(executable), '--data-dir', str(cloned), *arguments],
                                      check=True, text=True, capture_output=True, timeout=180).stdout
            # Exercise the settings editor against real Minecraft-generated properties.
            original_settings = next(item['settings'] for item in json.loads(command('list'))
                                     if item['id'] == args.server_id)
            players = 11 if original_settings['max_players'] != 11 else 12
            updated = json.loads(command('update-settings', args.server_id,
                                         '--name', original_settings['name'],
                                         '--memory-mb', str(original_settings['memory_mb']),
                                         '--port', str(original_settings['port']),
                                         '--max-players', str(players), '--difficulty', 'hard'))
            assert updated['settings']['max_players'] == players
            props = (server / 'server.properties').read_text()
            assert 'online-mode=true' in props and 'server-ip=127.0.0.1' in props
            assert 'view-distance=' in props, 'Minecraft-generated properties were lost'
            backup = json.loads(command('backup', args.server_id))
            entries = json.loads(command('list-backups', args.server_id))
            assert any(item['id'] == backup['id'] for item in entries)
            marker = server / 'world' / 'blockops-smoke-sentinel'
            marker.write_text('new world content')
            command('restore', args.server_id, backup['id'])
            assert digest(server / 'world' / 'level.dat') == before
            assert not marker.exists(), 'Restore did not replace the test world'
            retained = list(server.glob('world-before-restore-*'))
            assert any((path / marker.name).read_text() == 'new world content'
                       for path in retained if (path / marker.name).exists()), 'Previous world was not retained'
        assert digest(original_level) == before, 'Original world changed'
    print('PASS: real Minecraft settings updated; world backed up/restored; previous world retained; original unchanged')


if __name__ == '__main__':
    main()
