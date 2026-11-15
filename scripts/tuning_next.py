"""Choose a replay or bounded follow-up from a completed concurrency report."""
import json
import os
import sys


def next_action(report):
    rounds = report['rounds']
    if [r['workers'] for r in rounds] != [1, 2, 4, 1] or any(len(r['samples']) != 4 for r in rounds):
        raise ValueError('Incomplete measurement; do not dispatch from a partial report')
    selected = report['selected_workers']
    if selected is not None:
        if selected not in [1, 2, 4]:
            raise ValueError('Unsupported measured worker count')
        latest = next(r for r in reversed(rounds) if r['workers'] == selected)
        if not all(s['correct'] and s.get('error') is None for s in latest['samples']):
            raise ValueError('Selected worker count has no reliable final observation')
        return {'workers': selected}
    deadline = {180: 600, 600: 900}.get(report['timeout_seconds'])
    return {'deadline': deadline} if deadline else {}


if __name__ == '__main__':
    with open(sys.argv[1]) as source:
        report = json.load(source)
    action = next_action(report)
    for key, value in action.items():
        print(f'{key}={value}')
    summary = os.environ.get('GITHUB_STEP_SUMMARY')
    if summary:
        with open(summary, 'a') as out:
            out.write(f"Request deadline: {report['timeout_seconds']}s. Next action: {action or 'stop; no usable provider result'}.\n\n")
            out.write('| Round | Workers | Correct | Elapsed seconds |\n|---|---:|---:|---:|\n')
            for i, r in enumerate(report['rounds'], 1):
                out.write(f"| {i} | {r['workers']} | {sum(s['correct'] for s in r['samples'])}/4 | {r['seconds']:.1f} |\n")
