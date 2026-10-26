#!/usr/bin/env python3
"""Durable completion marker: systemd treats some signal exits as clean stops."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def main():
    marker=Path(sys.argv[1]); command=sys.argv[2:]
    child=subprocess.Popen(command)
    forwarded=[]
    def forward(sig, _):
        if not forwarded:
            forwarded.append(sig)
            try: os.kill(child.pid, sig)
            except ProcessLookupError: pass
    signal.signal(signal.SIGTERM,forward)
    signal.signal(signal.SIGINT,forward)
    marker.with_suffix('.started.json').write_text(json.dumps({'child_pid':child.pid})+'\n')
    code=child.wait()
    marker.write_text(json.dumps({'exit_code':code,'forwarded_signal':forwarded,
                                 'finished_utc':time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime())})+'\n')
    return code if code>=0 else 128-code


if __name__=='__main__': raise SystemExit(main())
