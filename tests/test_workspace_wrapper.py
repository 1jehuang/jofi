"""Regression tests for the installed Bash launcher's workspace restoration.

Run: python3 -m unittest discover -s tests -p 'test_*.py'
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

WRAPPER = Path(__file__).resolve().parents[1] / 'contrib/jofi-workspace'


class WorkspaceWrapperTests(unittest.TestCase):
    def run_wrapper(self, move=False, status=0):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            niri = root / 'niri'
            niri.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys
root = pathlib.Path(os.environ['FIXTURE'])
args = sys.argv[1:]
if args == ['msg', '-j', 'focused-window']:
    print(json.dumps({'id': 11, 'workspace_id': 4, 'pid': 100, 'app_id': 'kitty'}))
elif args == ['msg', '-j', 'workspaces']:
    moved = (root / 'moved').exists()
    print(json.dumps([
        {'id': 4, 'idx': 2 if not moved else 3, 'name': '4', 'is_focused': not moved},
        {'id': 2, 'idx': 4, 'name': '2', 'is_focused': moved}]))
else:
    with (root / 'actions').open('a') as f:
        f.write(json.dumps(args) + '\\n')
''')
            niri.chmod(0o755)
            launcher = root / 'launcher'
            launcher.write_text('#!/bin/bash\n'
                                + ('touch "$FIXTURE/moved"\n' if move else '')
                                + f'exit {status}\n')
            launcher.chmod(0o755)
            env = dict(os.environ, PATH=f'{root}:{os.environ["PATH"]}',
                       FIXTURE=str(root), JOFI_BIN=str(launcher))
            result = subprocess.run(['bash', str(WRAPPER)], env=env, timeout=5)
            actions = root / 'actions'
            return result.returncode, ([json.loads(line) for line in actions.read_text().splitlines()]
                                       if actions.exists() else [])

    def test_same_workspace_does_not_refocus_numeric_name(self):
        self.assertEqual(self.run_wrapper(), (0, []))

    def test_restores_stable_id_using_latest_index_not_numeric_name(self):
        self.assertEqual(self.run_wrapper(move=True),
                         (0, [['msg', 'action', 'focus-workspace', '3']]))

    def test_cancel_does_not_change_focus(self):
        self.assertEqual(self.run_wrapper(move=True, status=1), (1, []))


if __name__ == '__main__':
    unittest.main()
