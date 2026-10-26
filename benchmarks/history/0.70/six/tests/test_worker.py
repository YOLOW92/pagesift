import json
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

WORKER=Path(__file__).resolve().parents[1]/'worker.py'


class Completion(unittest.TestCase):
    def test_failed_command_has_durable_nonzero_result(self):
        with tempfile.TemporaryDirectory() as tmp:
            marker=Path(tmp)/'done.json'
            p=subprocess.run([sys.executable,str(WORKER),str(marker),sys.executable,'-c','raise SystemExit(7)'])
            self.assertEqual(p.returncode,7)
            self.assertEqual(json.loads(marker.read_text())['exit_code'],7)

    def test_clean_signal_handler_is_still_recorded_as_interrupted(self):
        with tempfile.TemporaryDirectory() as tmp:
            marker=Path(tmp)/'done.json';ready=Path(tmp)/'ready'
            code='import signal,sys,time;from pathlib import Path;signal.signal(signal.SIGTERM,lambda *_:sys.exit(0));Path(sys.argv[1]).touch();time.sleep(20)'
            p=subprocess.Popen([sys.executable,str(WORKER),str(marker),sys.executable,'-c',code,str(ready)])
            try:
                end=time.monotonic()+5
                while not (ready.exists() and marker.with_suffix('.started.json').exists()):
                    if time.monotonic()>end: self.fail('worker did not become ready')
                    time.sleep(.01)
                p.send_signal(signal.SIGTERM);p.wait(timeout=5)
                result=json.loads(marker.read_text())
                self.assertEqual(result['exit_code'],0)
                self.assertEqual(result['forwarded_signal'],[signal.SIGTERM])
            finally:
                if p.poll() is None:p.kill();p.wait()


if __name__=='__main__':unittest.main()
