#!/usr/bin/env python3
"""Capture real Wayland launcher screens without selecting or launching an app.

Requires grim and Pillow. Run only when no other launcher capture is running:
  python3 bench/compare-appearance.py --jofi target/release/jofi
  python3 bench/compare-appearance.py --reference-dir ~/.jcode/scratch/appearance-OLD
Screenshots and JSON stay under ~/.jcode/scratch, never in the repository.
The default compares real tofi-drun and Jofi catalogs, which can differ in text
and ordering. --input FILE feeds supplied catalog text to plain tofi, not Jofi
(Jofi does not accept stdin). No keys are injected. Only child PIDs are stopped.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

from PIL import Image


def bands(values):
    result = []
    for value in sorted(set(values)):
        if result and value == result[-1][1] + 1:
            result[-1][1] = value
        else:
            result.append([value, value])
    return result


def measure(path):
    image = Image.open(path).convert('RGB')
    groups = {'white': [], 'orange': [], 'tomato': []}
    for y in range(image.height):
        for x in range(image.width):
            r, g, b = image.getpixel((x, y))
            # Bright neutral and theme-colored glyph interiors. Dimmed desktop
            # pixels cannot pass these thresholds with the configured #000A.
            if min(r, g, b) >= 200 and max(r, g, b) - min(r, g, b) <= 15:
                groups['white'].append((x, y))
            elif r >= 180 and 35 <= g <= 90 and b <= 35:
                groups['orange'].append((x, y))
            elif r >= 180 and 65 <= g <= 125 and 40 <= b <= 100:
                groups['tomato'].append((x, y))
    result = {'size_px': list(image.size), 'colors': {}}
    for name, pixels in groups.items():
        ys = bands(y for _, y in pixels)
        result['colors'][name] = {
            'pixel_count': len(pixels),
            'bounds_xyxy': [min(x for x, _ in pixels), min(y for _, y in pixels),
                            max(x for x, _ in pixels), max(y for _, y in pixels)] if pixels else None,
            'row_bands': [{'y': [lo, hi], 'height': hi - lo + 1,
                           'x': [min(x for x, y in pixels if lo <= y <= hi),
                                 max(x for x, y in pixels if lo <= y <= hi)]}
                          for lo, hi in ys],
        }
    return result


def capture(command, destination, delay, input_path=None, output=None):
    source = open(input_path, 'rb') if input_path else subprocess.DEVNULL
    try:
        with destination.with_suffix('.stderr').open('wb') as log:
            proc = subprocess.Popen(command, stdin=source, stdout=subprocess.DEVNULL, stderr=log)
            try:
                time.sleep(delay)
                if proc.poll() is not None:
                    raise RuntimeError(f'{command} exited before capture, see {log.name}')
                grim = ['grim'] + (['-o', output] if output else []) + [str(destination)]
                subprocess.run(grim, check=True, timeout=15)
            finally:
                if proc.poll() is None:
                    proc.terminate()
                try:
                    proc.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait()
    finally:
        if input_path:
            source.close()
    time.sleep(0.3)


def self_test():
    import tempfile
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / 'fixture.png'
        image = Image.new('RGB', (20, 30), (50, 50, 50))
        for y in (5, 6, 15, 16):
            for x in range(3, 9):
                image.putpixel((x, y), (255, 255, 255))
        image.putpixel((2, 2), (255, 69, 0))
        image.putpixel((4, 4), (255, 99, 71))
        image.save(path)
        result = measure(path)
        assert result['colors']['white']['bounds_xyxy'] == [3, 5, 8, 16]
        assert [row['y'] for row in result['colors']['white']['row_bands']] == [[5, 6], [15, 16]]
        assert result['colors']['orange']['pixel_count'] == 1
        assert result['colors']['tomato']['pixel_count'] == 1
    print('Measurement self-test passed')


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--jofi', default='target/release/jofi')
    parser.add_argument('--reference-dir', type=Path, help='Reuse tofi.png from a previous capture')
    parser.add_argument('--directory', type=Path, help='Private capture directory under ~/.jcode/scratch')
    parser.add_argument('--input', type=Path, help='Optional text input for plain tofi, Jofi still uses its real catalog')
    parser.add_argument('--output', help='Wayland output name passed to grim')
    parser.add_argument('--delay', type=float, default=1.0)
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    scratch = (Path.home() / '.jcode/scratch').resolve()
    directory = (args.directory or scratch / time.strftime('appearance-%Y%m%d-%H%M%S')).resolve()
    if not directory.is_relative_to(scratch):
        parser.error('Capture directory must be under ~/.jcode/scratch for privacy')
    if args.delay <= 0:
        parser.error('--delay must be positive')
    directory.mkdir(parents=True, exist_ok=False, mode=0o700)
    os.chmod(directory, 0o700)
    commands = {'jofi': [str(Path(args.jofi).resolve())],
                'tofi': ['tofi' if args.input else 'tofi-drun']}
    if args.reference_dir:
        shutil.copyfile(args.reference_dir / 'tofi.png', directory / 'tofi.png')
    else:
        capture(commands['tofi'], directory / 'tofi.png', args.delay, args.input, args.output)
    capture(commands['jofi'], directory / 'jofi.png', args.delay, output=args.output)
    config = Path.home() / '.config/tofi/config'
    binary = Path(args.jofi).resolve()
    result = {'commands': commands, 'jofi_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'tofi_config': config.read_text() if config.exists() else None,
              'reference_reused': str(args.reference_dir) if args.reference_dir else None,
              'method': 'Real launcher screens, no key injection or app selection, color thresholds on physical screenshot pixels; catalogs/order may differ. Background must be dim enough not to pass foreground thresholds.',
              'screens': {name: measure(directory / f'{name}.png') for name in commands}}
    (directory / 'measurements.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2))
    print(f'Private artifacts: {directory}')


if __name__ == '__main__':
    main()
