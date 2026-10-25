import csv
import os
from pathlib import Path
import statistics
import subprocess
import sys


def main():
    if len(sys.argv) != 2:
        raise SystemExit('usage: run.py <new-output-directory> (run from repository root)')
    root = Path(sys.argv[1])
    suffix = '.exe' if os.name == 'nt' else ''
    exe = Path('target/release/examples/multi_machine' + suffix).resolve()
    if not exe.is_file():
        raise SystemExit('Build the Release multi_machine example before running the matrix')
    root.mkdir()
    cases = [(n, s, False, False) for n in [1, 16, 64] for s in [0, 8]]
    cases += [(n, s, False, True) for n in [16, 64] for s in [0, 8]]
    cases += [(64, s, True, False) for s in [0, 8]]
    all_rows = []
    references = {}
    for repeat in range(3):
        for n, subscribers, shared, file in cases:
            for workers in ([0, 4] if repeat % 2 == 0 else [4, 0]):
                name = f'r{repeat}-{n}-{subscribers}-{int(shared)}-{int(file)}-{workers}'
                output = root / name
                args = [str(exe), str(output), '--machines', str(n),
                        '--subscribers', str(subscribers), '--workers', str(workers)]
                if shared:
                    args += ['--shared']
                if file:
                    args += ['--file']
                with (root / (name + '.txt')).open('w', encoding='utf-8') as log:
                    subprocess.run(args, stdout=log, stderr=subprocess.STDOUT, check=True)
                with (output / 'ticks.csv').open(newline='') as source:
                    rows = list(csv.DictReader(source))
                for row in rows:
                    row['repeat'] = repeat
                    all_rows.append(row)
                key = n, subscribers, shared
                artifacts = {p.name: p.read_bytes() for p in output.glob('*.bin')}
                if key in references:
                    assert artifacts == references[key], name
                else:
                    references[key] = artifacts
                measured = [int(row['drive_ns']) for row in rows if row['measured'] == 'true']
                print(name, 'p50_ms', round(statistics.median(measured) / 1e6, 3), flush=True)
    with (root / 'ticks.csv').open('w', newline='') as target:
        writer = csv.DictWriter(target, fieldnames=list(all_rows[0]))
        writer.writeheader()
        writer.writerows(all_rows)
    print('Exact journals, snapshots and per-subscriber streams match '
          'across modes/backends/repeats', flush=True)


if __name__ == '__main__':
    main()
