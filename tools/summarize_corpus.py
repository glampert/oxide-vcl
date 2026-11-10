#!/usr/bin/env python3
"""Create a compact baseline from the explicit attempt selection and replay."""
import argparse
from collections import Counter
import json
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--corpus', type=Path, default=Path('local/evidence/corpus'))
    parser.add_argument('--comparison', type=Path,
                        default=Path('local/evidence/corpus/item2-selected-comparison'))
    parser.add_argument('--output-stem', default='baseline',
                        help='name for separate JSON/table snapshots')
    args = parser.parse_args()
    selection = json.loads((args.corpus / 'attempt-selection.json').read_text())
    report = json.loads((args.comparison / 'report.json').read_text())
    comparisons = {r['name']: r for r in report['cases']}
    if set(comparisons) != {r['name'] for r in selection}:
        raise SystemExit('comparison does not cover the full selected input inventory')
    if report['candidate_changed_during_run']:
        raise SystemExit('candidate changed during replay; rerun the comparison')
    rows = []
    for selected in selection:
        name = selected['name']
        replay = comparisons.get(name, {})
        original = (json.loads((Path(selected['selected']) / 'result.json').read_text())
                    if selected['selected'] else {})
        assembly = replay.get('assembly', {})
        diagnostic = args.comparison / name / 'stderr.bin'
        rows.append({'name': name, 'original_completed': selected['original_completed'],
                     'selected_original_directory': selected['selected'],
                     'alarm_scale': original.get('time_scale'),
                     'attempt_count': len(selected['attempts']),
                     'original_exit_status': original.get('exit_status'),
                     'candidate_exit_status': replay.get('candidate_exit_status'),
                     'classification': replay.get('classification', 'not_compared'),
                     'stdout_bytes_equal': replay.get('stdout_bytes_equal'),
                     'stderr_bytes_equal': replay.get('stderr_bytes_equal'),
                     'assembly_comparable': assembly.get('assembly_comparable'),
                     'original_pairs': assembly.get('expected_metrics', {}).get('issue_pairs'),
                     'candidate_pairs': assembly.get('actual_metrics', {}).get('issue_pairs'),
                     'candidate_diagnostic': diagnostic.read_text(errors='replace').strip()
                     if diagnostic.exists() else None})
    summary = {'total': len(rows), 'original_completed': sum(r['original_completed'] for r in rows),
               'alarm_scales': sorted({r['alarm_scale'] for r in rows if r['alarm_scale'] is not None}),
               'classifications': dict(Counter(r['classification'] for r in rows)),
               'both_streams_byte_equal': sum(r['stdout_bytes_equal'] is True
                                             and r['stderr_bytes_equal'] is True for r in rows)}
    result = {'candidate_sha256': report['candidate_sha256'],
              'candidate_changed_during_run': report['candidate_changed_during_run'],
              'summary': summary, 'cases': rows}
    (args.corpus / (args.output_stem + '.json')).write_text(json.dumps(result, indent=2) + '\n')
    lines = ['| Program | Original status | Rust status | Classification | Original/Rust pairs |',
             '| --- | ---: | ---: | --- | ---: |']
    for row in rows:
        original = row['original_exit_status'] if row['original_completed'] else 'unknown'
        pairs = (f"{row['original_pairs']}/{row['candidate_pairs']}"
                 if row['original_pairs'] is not None else '—')
        lines.append(f"| `{row['name']}` | {original} | {row['candidate_exit_status']} | {row['classification']} | {pairs} |")
    (args.corpus / (args.output_stem + '-table.md')).write_text('\n'.join(lines) + '\n')
    print(json.dumps(summary, indent=2))


if __name__ == '__main__':
    main()
