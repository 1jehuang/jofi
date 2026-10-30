#!/usr/bin/env python3
"""Exercise actual Jofi keyboard search and fixture launching in an isolated niri.

Requires installed Xvfb, xdotool, niri, and gio. No keys or windows reach the real
user desktop. A private Xvfb display hosts nested niri without --session. All
launchable entries, runtime sockets, history and cache are temporary fixtures.
Selecting a result executes only a harmless Python marker through real gio.
Artifacts stay in ~/.jcode/scratch by default. Only owned child PIDs are stopped.
"""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import secrets
import selectors
import shutil
import struct
import subprocess
import sys
import tempfile
import time


def until(check, label, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.02)
    raise RuntimeError(f"Timed out waiting for {label}")


def stop(process):
    if process is not None and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=3)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--jofi', default='target/release/jofi')
    parser.add_argument('--output-dir')
    args = parser.parse_args()
    for tool in ['Xvfb', 'xdotool', 'niri', 'gio']:
        if shutil.which(tool) is None:
            parser.error(f"Required executable not installed: {tool}")
    original = Path(args.jofi).resolve(strict=True)
    scratch = Path(os.environ.get('JCODE_SCRATCH_DIR', Path.home() / '.jcode/scratch'))
    scratch.mkdir(parents=True, exist_ok=True)
    if args.output_dir:
        root = Path(args.output_dir).resolve()
        root.mkdir(parents=True, mode=0o700)
    else:
        root = Path(tempfile.mkdtemp(prefix='jofi-search-workflow-', dir=scratch))
    runtime = root / 'runtime'
    runtime.mkdir(mode=0o700)
    for sub in ['data/applications', 'empty/applications', 'scripts', 'state', 'cache', 'config']:
        (root / sub).mkdir(parents=True)
    binary = root / 'jofi'
    shutil.copy2(original, binary)
    binary.chmod(0o755)
    marker = root / 'launches.jsonl'
    helper = root / 'record-launch.py'
    helper.write_text("import json, pathlib, sys\nwith pathlib.Path(sys.argv[1]).open('a') as f:\n    f.write(json.dumps(sys.argv[2]) + '\\n')\n")
    names = ['Google Chrome', 'Chromium Browser', 'Calculator', 'Visual Studio Code',
             'Firefox', 'Terminal', 'Terminal Profile', 'Café Notes', 'Code Reference', 'NetworkManager']
    entries = {}
    for i, name in enumerate(names):
        path = root / 'data/applications' / f'app-{i}.desktop'
        # Desktop Exec arguments are individually quoted, not passed to a shell.
        quoted = lambda s: '"' + str(s).replace('\\', '\\\\').replace('"', '\\"') + '"'
        command = ' '.join(map(quoted, [sys.executable, helper, marker, name]))
        path.write_text(f'[Desktop Entry]\nType=Application\nName={name}\nExec={command}\nTerminal=false\n')
        entries[name] = path
    config = root / 'niri.kdl'
    config.write_text('input { keyboard { xkb { layout "us"; }; }; }\nhotkey-overlay { skip-at-startup; }\nanimations { off; }\n')
    # FamilyWild Xauthority cookie authorizes only clients that receive this
    # private file. No TCP listener, no access to the active desktop's X server.
    authority = root / 'Xauthority'
    cookie = secrets.token_bytes(16)
    authority.write_bytes(struct.pack('!H', 65535) + b''.join(struct.pack('!H', len(s)) + s for s in [b'', b'', b'MIT-MAGIC-COOKIE-1', cookie]))
    authority.chmod(0o600)
    env = dict(os.environ)
    for key in ['WAYLAND_DISPLAY', 'WAYLAND_SOCKET', 'NIRI_SOCKET', 'DBUS_SESSION_BUS_ADDRESS', 'XAUTHORITY', 'DISPLAY']:
        env.pop(key, None)
    env.update(XDG_RUNTIME_DIR=str(runtime), XDG_DATA_HOME=str(root / 'data'),
               XDG_DATA_DIRS=str(root / 'empty'), XDG_STATE_HOME=str(root / 'state'),
               XDG_CACHE_HOME=str(root / 'cache'), XDG_CONFIG_HOME=str(root / 'config'),
               JOFI_SCRIPTS_DIR=str(root / 'scripts'), XDG_CURRENT_DESKTOP='niri',
               XAUTHORITY=str(authority), LIBGL_ALWAYS_SOFTWARE='1', GALLIUM_DRIVER='llvmpipe')
    xvfb = niri = launcher = None
    logs = []
    result = {'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'source_binary': str(original), 'artifact_dir': str(root), 'cases': []}
    try:
        read_fd, write_fd = os.pipe()
        xvfb_log = (root / 'xvfb.log').open('wb')
        logs.append(xvfb_log)
        xvfb = subprocess.Popen(['Xvfb', '-displayfd', str(write_fd), '-screen', '0', '1280x900x24',
                                 '-nolisten', 'tcp', '-noreset', '-auth', str(authority)],
                                env=env, pass_fds=(write_fd,), stdout=xvfb_log, stderr=subprocess.STDOUT)
        os.close(write_fd)
        with selectors.DefaultSelector() as selector:
            selector.register(read_fd, selectors.EVENT_READ)
            if not selector.select(10):
                raise RuntimeError('Xvfb did not allocate a private display')
            display = os.read(read_fd, 64).decode().strip()
        os.close(read_fd)
        if not display.isdecimal():
            raise RuntimeError(f'Invalid Xvfb display {display!r}')
        env['DISPLAY'] = ':' + display
        result['private_display'] = env['DISPLAY']
        niri_log = (root / 'niri.log').open('wb')
        logs.append(niri_log)
        niri = subprocess.Popen(['niri', '--config', str(config)], env=env,
                                stdout=niri_log, stderr=subprocess.STDOUT)
        def socket_ready():
            if niri.poll() is not None:
                raise RuntimeError(f'niri exited: {(root / "niri.log").read_text()[-6000:]}')
            sockets = [p for p in runtime.glob('wayland-*') if not p.name.endswith('.lock')]
            ipc = list(runtime.glob('niri.*.sock'))
            return (sockets[0], ipc[0]) if sockets and ipc else None
        wayland, ipc = until(socket_ready, 'private niri sockets')
        env['WAYLAND_DISPLAY'] = wayland.name
        env['NIRI_SOCKET'] = str(ipc)
        # Every xdotool subprocess is pinned to the private DISPLAY and cookie.
        # No fallback to the active desktop's display is possible.
        def xdo(*arguments):
            assert env['DISPLAY'] == ':' + display and env['XAUTHORITY'] == str(authority)
            return subprocess.check_output(['xdotool', *arguments], env=env, stderr=subprocess.STDOUT, timeout=5).decode().strip()
        def find_window():
            found = subprocess.run(['xdotool', 'search', '--name', '^niri$'], env=env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5)
            return found.stdout.decode().splitlines()[0] if found.returncode == 0 else None
        window = until(find_window, 'nested niri X11 window')
        xdo('windowfocus', '--sync', window)
        def launches():
            return [json.loads(line) for line in marker.read_text().splitlines()] if marker.exists() else []
        def start():
            nonlocal launcher
            log = (root / f'jofi-{len(result["cases"])}.log').open('wb')
            logs.append(log)
            launcher = subprocess.Popen([str(binary)], env=dict(env, WAYLAND_DEBUG='client'), stdout=log, stderr=subprocess.STDOUT)
            def focused():
                if launcher.poll() is not None:
                    raise RuntimeError(f'Jofi exited before keyboard focus, see {log.name}')
                return re.search(rb'wl_keyboard[#@]\d+\.enter(?:, )?\(', Path(log.name).read_bytes())
            until(focused, 'Jofi keyboard focus')
            time.sleep(0.05)
        def type_text(text):
            if text:
                xdo('type', '--clearmodifiers', '--delay', '8', text)
                time.sleep(0.06)
        def key(*keys):
            xdo('key', '--clearmodifiers', *keys)
            time.sleep(0.04)
        def choose(label, query, expected, keys=()):
            before = launches()
            start()
            type_text(query)
            if keys:
                key(*keys)
            key('Return')
            until(lambda: len(launches()) == len(before) + 1, label + ' fixture launch')
            actual = launches()[-1]
            assert actual == expected, f'{label}: expected {expected!r}, got {actual!r}'
            launcher.wait(timeout=5)
            assert launcher.returncode == 0, f'{label}: exit {launcher.returncode}'
            history = (root / 'state/jofi/recent').read_text().splitlines()
            assert history[0] == str(entries[expected]), f'{label}: history identity mismatch'
            usage = {}
            for line in (root / 'state/jofi/usage.tsv').read_text().splitlines():
                count, last, imported, identity = line.split('\t')
                assert int(last) > 0 and int(imported) == 0, f'{label}: invalid usage metadata'
                usage[identity] = int(count)
            expected_usage = {}
            for name in launches():
                identity = str(entries[name])
                expected_usage[identity] = expected_usage.get(identity, 0) + 1
            assert usage == expected_usage, f'{label}: incorrect persisted launch frequencies'
            result['cases'].append({'case': label, 'query': query, 'selected': actual, 'passed': True})
        choose('transposed typo', 'chrmoe', 'Google Chrome')
        choose('unordered terms with typo', 'cdoe visual', 'Visual Studio Code')
        choose('two substitutions', 'calxulatxr', 'Calculator')
        choose('initial abbreviation', 'ffx', 'Firefox')
        choose('accent-insensitive match', 'cafe', 'Café Notes')
        choose('backspace repairs query', 'chrmoex', 'Google Chrome', ('BackSpace',))
        choose('down selects next result', 'term', 'Terminal Profile', ('Down',))
        choose('up wraps to final result', 'term', 'Terminal Profile', ('Up',))
        choose('Tab selects next result', 'term', 'Terminal Profile', ('Tab',))
        choose('Shift+Tab selects previous result', 'term', 'Terminal Profile', ('shift+Tab',))
        choose('blank query promotes most-used launcher', '', 'Terminal Profile')
        # Enter on an empty result must not close or accidentally launch the
        # previously selected app. Ctrl+U then searches from a clean selection.
        before = launches()
        start()
        type_text('zzzzzz')
        key('Return')
        assert launcher.poll() is None and launches() == before, 'Enter with no matches launched or closed'
        key('ctrl+u')
        type_text('ffx')
        key('Return')
        until(lambda: len(launches()) == len(before) + 1, 'Ctrl+U recovery')
        assert launches()[-1] == 'Firefox'
        launcher.wait(timeout=5)
        result['cases'].append({'case': 'empty-result Enter and Ctrl+U recovery', 'passed': True})
        before = launches()
        history_before = (root / 'state/jofi/recent').read_bytes()
        usage_before = (root / 'state/jofi/usage.tsv').read_bytes()
        start()
        type_text('^')
        key('Return')
        assert launcher.poll() is None and launches() == before, 'Literal punctuation incorrectly selected an app'
        key('Escape')
        launcher.wait(timeout=5)
        assert launches() == before and (root / 'state/jofi/recent').read_bytes() == history_before
        assert (root / 'state/jofi/usage.tsv').read_bytes() == usage_before, 'Cancellation changed usage counts'
        result['cases'].append({'case': 'literal punctuation and Escape cancellation', 'passed': True})
        result['passed'] = True
    except Exception as error:
        result.update(passed=False, error=str(error))
        raise
    finally:
        stop(launcher)
        stop(niri)
        stop(xvfb)
        for log in logs:
            log.close()
        (root / 'results.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result, indent=2), flush=True)
    return 0


if __name__ == '__main__':
    sys.exit(main())
