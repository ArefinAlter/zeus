"""Build the isolated read-only Windows SDK2 broker with an existing MinGW-w64 compiler."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cc', default=shutil.which('gcc') or 'gcc')
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    source = Path(__file__).resolve().parent
    args.out.mkdir(parents=True, exist_ok=True)
    executable = args.out.resolve() / 'zeus-everything-query.exe'
    command = [args.cc, '-std=c11', '-O2', '-municode', '-Wl,--no-insert-timestamp',
               str(source / 'broker.c'), '-o', str(executable), '-luser32']
    result = subprocess.run(command, capture_output=True, text=True, encoding='utf-8', errors='replace')
    (args.out / 'build.log').write_text(result.stdout + result.stderr, encoding='utf-8')
    result.check_returncode()
    if os.name == 'nt':
        probe = subprocess.run([str(executable), '--protocol-version'], capture_output=True,
                               text=True, timeout=5, check=True)
        if probe.stdout.strip() != 'zeus-everything-sdk2-v1':
            raise RuntimeError('Unexpected broker protocol')
    shutil.copyfile(source / 'LICENSE-SDK.txt', args.out / 'LICENSE-SDK.txt')
    files = [source / 'broker.c'] + sorted((source / 'sdk').rglob('*'))
    receipt = {'protocol': 'zeus-everything-sdk2-v1', 'sha256': digest(executable),
               'bytes': executable.stat().st_size,
               'source_sha256': {str(p.relative_to(source)): digest(p) for p in files if p.is_file()}}
    (args.out / 'build.json').write_text(json.dumps(receipt, indent=2), encoding='utf-8')
    print(json.dumps(receipt))


if __name__ == '__main__':
    main()
