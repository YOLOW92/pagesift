"""Require exactly one result for every unique input ID."""
import json


def validate(manifest, output):
    with manifest.open() as f:
        wanted = [r['id'] for r in map(json.loads, f)]
    with output.open() as f:
        got = [r['id'] for r in map(json.loads, f)]
    if len(wanted) != len(set(wanted)):
        raise RuntimeError(f'duplicate input sample IDs: {manifest}')
    if len(got) != len(set(got)) or set(got) != set(wanted):
        raise RuntimeError(f'incomplete/duplicate sample IDs: {output}')
