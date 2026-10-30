#!/usr/bin/env python3
"""Compare real launcher first-buffer submission on the current Wayland session.
WAYLAND_DEBUG adds instrumentation overhead. This is not presentation latency.
No keys are injected, no apps selected, and only child PIDs are terminated.
"""
import argparse
import json
import os
from pathlib import Path
import random
import re
import selectors
import statistics
import subprocess
import time


def memory(pid):
    result = {}
    for filename, keys in [('status', ('VmRSS', 'VmHWM')), ('smaps_rollup', ('Pss', 'Private_Clean', 'Private_Dirty'))]:
        try:
            for line in Path(f'/proc/{pid}/{filename}').read_text().splitlines():
                key, _, rest = line.partition(':')
                if key in keys:
                    result[key + '_kib'] = int(rest.split()[0])
        except (FileNotFoundError, PermissionError):
            pass
    return result


def trial(command):
    env = dict(os.environ, WAYLAND_DEBUG='client')
    start = time.perf_counter_ns()
    proc = subprocess.Popen(command, env=env, stdin=subprocess.DEVNULL,
                            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    selector = selectors.DefaultSelector()
    selector.register(proc.stderr, selectors.EVENT_READ)
    pending = b''
    attached = set()
    buffers = {}
    parents = {}
    subsurfaces = {}
    synchronized = set()
    waiting_parents = set()
    submitted = None
    content_root = None
    keyboard_enters = {}
    def keyboard_event(line, now):
        event = re.search(r'wl_keyboard[#@]\d+\.enter(?:, )?\(\d+,\s*wl_surface[#@](\d+)', line)
        if event:
            keyboard_enters[event.group(1)] = (now - start) / 1_000_000
    log = []
    try:
        while time.perf_counter_ns() - start < 5_000_000_000:
            if not selector.select(0.1):
                if proc.poll() is not None:
                    break
                continue
            chunk = os.read(proc.stderr.fileno(), 65536)
            now = time.perf_counter_ns()
            if not chunk:
                break
            pending += chunk
            while b'\n' in pending:
                line, pending = pending.split(b'\n', 1)
                line = line.decode(errors='replace')
                log.append(line)
                keyboard_event(line, now)
                child = re.search(r'\.get_subsurface\((?:new id )?wl_subsurface[#@](\d+),\s*wl_surface[#@](\d+),\s*wl_surface[#@](\d+)\)', line)
                if child:
                    subsurfaces[child.group(1)] = child.group(2)
                    parents[child.group(2)] = child.group(3)
                    synchronized.add(child.group(2))
                desync = re.search(r'wl_subsurface[#@](\d+)\.set_desync', line)
                if desync:
                    synchronized.discard(subsurfaces.get(desync.group(1)))
                buffer = re.search(r'\.create_buffer\((?:new id )?wl_buffer[#@](\d+),\s*\d+,\s*(\d+),\s*(\d+),', line)
                if buffer:
                    buffers[buffer.group(1)] = (int(buffer.group(2)), int(buffer.group(3)))
                match = re.search(r'-> wl_surface[#@](\d+)\.attach\(wl_buffer[#@](\d+)', line)
                # Tofi submits a stretched 1x1 background before rendering text.
                if match and min(buffers.get(match.group(2), (0, 0))) > 1:
                    attached.add(match.group(1))
                match = re.search(r'-> wl_surface[#@](\d+)\.commit\(', line)
                if match:
                    surface = match.group(1)
                    if surface in waiting_parents:
                        content_root = surface
                        submitted = (now - start) / 1_000_000
                        break
                    if surface in attached:
                        if surface in synchronized:
                            waiting_parents.add(parents[surface])
                        else:
                            content_root = surface
                            submitted = (now - start) / 1_000_000
                            break
            if submitted is not None:
                break
        if submitted is None:
            raise RuntimeError(f'No first-buffer commit for {command}:\n' + '\n'.join(log[-35:]))
        while b'\n' in pending:
            line, pending = pending.split(b'\n', 1)
            keyboard_event(line.decode(errors='replace'), now)
        initial = memory(proc.pid)
        # Drain debug output while the compositor consumes the initial buffer.
        until = time.monotonic() + 0.1
        while time.monotonic() < until:
            if selector.select(0.01):
                pending += os.read(proc.stderr.fileno(), 65536)
            now = time.perf_counter_ns()
            while b'\n' in pending:
                line, pending = pending.split(b'\n', 1)
                keyboard_event(line.decode(errors='replace'), now)
        settled = memory(proc.pid)
        return {'first_buffer_ms': submitted, 'keyboard_and_content_ms': max(submitted, keyboard_enters[content_root]) if content_root in keyboard_enters else None, 'initial_memory': initial, 'settled_memory': settled}
    finally:
        selector.close()
        if proc.poll() is None:
            proc.terminate()
        try:
            proc.wait(timeout=2)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        proc.stderr.close()


def summarize(rows):
    latency = sorted(r['first_buffer_ms'] for r in rows)
    result = {'trials': len(rows), 'median_ms': statistics.median(latency),
              'p95_ms': latency[max(0, __import__('math').ceil(len(latency) * .95) - 1)],
              'min_ms': min(latency), 'max_ms': max(latency),
              'mean_ms': statistics.mean(latency), 'stdev_ms': statistics.stdev(latency) if len(latency) > 1 else 0}
    ready = [r['keyboard_and_content_ms'] for r in rows if r.get('keyboard_and_content_ms') is not None]
    if ready:
        result['median_keyboard_and_content_ms'] = statistics.median(ready)
    for key in ('VmRSS_kib', 'VmHWM_kib', 'Pss_kib'):
        values = [r['settled_memory'][key] for r in rows if key in r['settled_memory']]
        if values:
            result['median_' + key] = statistics.median(values)
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--runs', type=int, default=30)
    parser.add_argument('--output', required=True)
    parser.add_argument('--warmup', type=int, default=3)
    parser.add_argument('--jofi', default=str(Path.home() / '.local/bin/jofi'))
    parser.add_argument('--baseline', help='Optional preserved previous jofi binary')
    args = parser.parse_args()
    commands = {'jofi': [args.jofi], 'tofi-drun': ['/usr/bin/tofi-drun']}
    if args.baseline:
        commands['jofi-before'] = [args.baseline]
    rows = {name: [] for name in commands}
    rng = random.Random(20261030)
    for round_number in range(args.warmup + args.runs):
        order = list(commands)
        rng.shuffle(order)
        for name in order:
            row = trial(commands[name])
            print(f'{round_number + 1}/{args.runs + args.warmup} {name}: {row["first_buffer_ms"]:.2f} ms', flush=True)
            if round_number >= args.warmup:
                rows[name].append(row)
    result = {'method': 'spawn to first content buffer (>1px in both dimensions) attachment followed by wl_surface commit, waits for synchronized subsurface parent commit, excludes 1x1 background placeholders, WAYLAND_DEBUG=client, current session/theme, warm filesystem caches; memory 100ms after commit',
              'commands': commands, 'warmup_per_app': args.warmup,
              'summary': {name: summarize(data) for name, data in rows.items()}, 'raw': rows}
    Path(args.output).write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result['summary'], indent=2))


if __name__ == '__main__':
    main()
