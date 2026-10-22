import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


def module(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / (name + '.py'))
    loaded = importlib.util.module_from_spec(spec); spec.loader.exec_module(loaded)
    return loaded


class Protocol(unittest.TestCase):
    def test_version_input_and_environment_change_identity(self):
        runner = module('run')
        with tempfile.TemporaryDirectory() as tmp:
            f = Path(tmp) / 'input'; f.write_text('one')
            a = runner.identity(['/usr/bin/true'], [f], {'THREADS': '1'})[0]
            self.assertEqual(a, runner.identity(['/usr/bin/true'], [f], {'THREADS': '1'})[0])
            f.write_text('two')
            b = runner.identity(['/usr/bin/true'], [f], {'THREADS': '1'})[0]
            c = runner.identity(['/usr/bin/true'], [f], {'THREADS': '2'})[0]
            self.assertNotEqual(a, b); self.assertNotEqual(b, c)

    def test_equal_line_count_with_duplicate_id_is_not_complete(self):
        baseline = module('baseline')
        with tempfile.TemporaryDirectory() as tmp:
            manifest = Path(tmp) / 'manifest'; output = Path(tmp) / 'output'
            manifest.write_text('{"id":"a"}\n{"id":"b"}\n')
            output.write_text('{"id":"a"}\n{"id":"a"}\n')
            with self.assertRaises(RuntimeError): baseline.validate(manifest, output)
            output.write_text('{"id":"a"}\n')
            with self.assertRaises(RuntimeError): baseline.validate(manifest, output)
            output.write_text('{"id":"b"}\n{"id":"a"}\n')
            baseline.validate(manifest, output)


if __name__ == '__main__': unittest.main()
