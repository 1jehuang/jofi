#!/usr/bin/env python3
"""Exercise the workspace wrapper without touching a real compositor."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

WRAPPER = Path(__file__).resolve().parents[1] / 'contrib/jofi-workspace'


class WorkspaceWrapper(unittest.TestCase):
    def test_context_arguments_and_workspace_restoration(self):
        with tempfile.TemporaryDirectory(prefix='jofi-wrapper-') as temporary:
            root = Path(temporary)
            niri = root / 'niri'
            niri.write_text('''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
if sys.argv[1:] == ['msg', '-j', 'focused-window']:
    print(json.dumps({'id': 42, 'pid': 123, 'app_id': 'test.app', 'workspace_id': 7}))
elif sys.argv[1:] == ['msg', '-j', 'workspaces']:
    print(json.dumps([{'is_focused': True, 'name': 'My workspace', 'idx': 7}]))
else:
    Path(os.environ['CALLS']).write_text(json.dumps(sys.argv[1:]))
''')
            launcher = root / 'fixture-launcher'
            launcher.write_text('''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
keys = ['JSTACK_ORIGIN_WINDOW_ID', 'JSTACK_ORIGIN_WINDOW_PID', 'JSTACK_ORIGIN_WINDOW_APP_ID', 'JSTACK_ORIGIN_WORKSPACE_ID']
Path(os.environ['CAPTURE']).write_text(json.dumps({'context': {k: os.environ.get(k) for k in keys}, 'args': sys.argv[1:]}))
sys.exit(int(os.environ.get('STATUS', '0')))
''')
            niri.chmod(0o755)
            launcher.chmod(0o755)
            environment = dict(os.environ, PATH=f'{root}:{os.environ["PATH"]}',
                               JOFI_BIN=str(launcher), CAPTURE=str(root / 'capture'),
                               CALLS=str(root / 'calls'))
            for status in (0, 1):
                with self.subTest(status=status):
                    (root / 'calls').unlink(missing_ok=True)
                    result = subprocess.run([str(WRAPPER), 'argument with spaces', '$literal'],
                                            env=dict(environment, STATUS=str(status)), check=False)
                    self.assertEqual(result.returncode, status)
                    captured = json.loads((root / 'capture').read_text())
                    self.assertEqual(captured['args'], ['argument with spaces', '$literal'])
                    self.assertEqual(list(captured['context'].values()), ['42', '123', 'test.app', '7'])
                    if status == 0:
                        self.assertEqual(json.loads((root / 'calls').read_text()),
                                         ['msg', 'action', 'focus-workspace', 'My workspace'])
                    else:
                        self.assertFalse((root / 'calls').exists())


if __name__ == '__main__':
    unittest.main()
