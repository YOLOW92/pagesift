#!/usr/bin/env python3
"""Run one benchmark/build in a bounded systemd user service; preserve every outcome."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time
import uuid

ROOT = Path(__file__).resolve().parents[4]


def digest(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()


def available_mb():
    for line in Path('/proc/meminfo').read_text().splitlines():
        if line.startswith('MemAvailable:'):
            return int(line.split()[1]) // 1024
    raise RuntimeError('cannot measure available memory')


def preflight(path, reserve_mb, reserve_disk_gb):
    mem = available_mb()
    disk = shutil.disk_usage(path).free / 2**30
    if mem < reserve_mb or disk < reserve_disk_gb:
        raise RuntimeError(f'resource reserve: available {mem} MB, disk {disk:.2f} GiB')
    return {'available_mb': mem, 'free_disk_gib': disk}


def identity(command, inputs, env):
    executable = shutil.which(command[0])
    files = sorted({str(Path(p).resolve()) for p in inputs})
    if executable:
        files.append(str(Path(executable).resolve()))
    record = {'command': command, 'cwd': str(ROOT), 'env': env,
              'files': {p: digest(p) for p in files}, 'runner_sha256': digest(__file__)}
    return hashlib.sha256(json.dumps(record, sort_keys=True).encode()).hexdigest(), record


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--id', required=True)
    p.add_argument('--timeout', type=int, default=600)
    p.add_argument('--memory-mb', type=int, default=8192)
    p.add_argument('--cpus', default='0,1,2,3,4,5,6,7')
    p.add_argument('--cpu-quota', type=int, default=800)
    p.add_argument('--reserve-mb', type=int, default=1536)
    p.add_argument('--reserve-disk-gb', type=float, default=10)
    p.add_argument('--input', action='append', default=[])
    p.add_argument('--output', action='append', default=[])
    p.add_argument('--env', action='append', default=[])
    p.add_argument('command', nargs=argparse.REMAINDER)
    a = p.parse_args()
    command = a.command[1:] if a.command[:1] == ['--'] else a.command
    if not command or not re.fullmatch(r'[a-zA-Z0-9_-]+', a.id):
        p.error('a safe id and command are required')
    env = {'CARGO_BUILD_JOBS': '8', 'OMP_NUM_THREADS': '1', 'OPENBLAS_NUM_THREADS': '1',
           'MKL_NUM_THREADS': '1', 'RAYON_NUM_THREADS': '8', 'TOKIO_WORKER_THREADS': '8',
           'UV_THREADPOOL_SIZE': '8', 'NODE_OPTIONS': '--max-old-space-size=4096',
           'PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD': '1', 'PUPPETEER_SKIP_DOWNLOAD': '1'}
    for value in a.env:
        k, v = value.split('=', 1)
        env[k] = v
    run_id = time.strftime('%Y%m%dT%H%M%SZ', time.gmtime()) + '-' + a.id + '-' + uuid.uuid4().hex[:6]
    out = ROOT / 'benchmarks/history/0.70/six/results' / run_id
    out.mkdir(parents=True)
    rec = {'id': a.id, 'run_id': run_id, 'status': 'preflight', 'command': command,
           'limits': {'memory_mb': a.memory_mb, 'cpu_quota_percent': a.cpu_quota, 'cpus': a.cpus,
                      'swap_mb': 0, 'timeout_s': a.timeout}, 'output_files': a.output}
    status_file = out / 'run.json'

    def save():
        status_file.write_text(json.dumps(rec, indent=2) + '\n')

    unit = 'pagesift-six-' + uuid.uuid4().hex[:12]
    proc = None
    t0 = time.monotonic()
    try:
        rec['preflight'] = preflight(ROOT, a.reserve_mb, a.reserve_disk_gb)
        rec['identity'], rec['identity_record'] = identity(command, a.input + [Path(__file__).with_name('worker.py')], env)
        rec['git_sha'] = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
        rec['git_dirty'] = subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT, text=True)
        patch = subprocess.check_output(['git', 'diff', '--binary', 'HEAD'], cwd=ROOT)
        (out / 'source.patch').write_bytes(patch)
        rec['source_patch_sha256'] = hashlib.sha256(patch).hexdigest()
        # Every task shares this slice: concurrent preparation cannot exceed the
        # user's aggregate eight-thread / 8 GiB allowance either.
        subprocess.run(['systemctl', '--user', 'set-property', '--runtime', 'pagesift-six.slice',
                        'CPUQuota=800%', 'MemoryMax=8G', 'MemorySwapMax=0', 'AllowedCPUs=0-7'],
                       check=True, capture_output=True)
        args = ['systemd-run', '--user', '--wait', '--pipe', '--unit=' + unit,
                '--slice=pagesift-six.slice',
                '-p', f'CPUQuota={a.cpu_quota}%', '-p', f'AllowedCPUs={a.cpus}',
                '-p', f'MemoryMax={a.memory_mb}M', '-p', 'MemorySwapMax=0',
                '-p', 'Nice=10', '-p', 'IOSchedulingClass=idle',
                '-p', 'KillMode=control-group', '-p', 'TimeoutStopSec=90s', '-p', f'RuntimeMaxSec={a.timeout}s',
                '-p', 'WorkingDirectory=' + str(ROOT)]
        for k, v in env.items():
            args += ['--setenv=' + k + '=' + v]
        # systemd services do not inherit the caller's full environment.
        marker = out / 'worker-result.json'
        args += ['--setenv=PATH=' + os.environ['PATH'], '--', '/usr/bin/python3',
                 str(Path(__file__).with_name('worker.py')), str(marker)] + command
        rec['status'] = 'running'
        save()
        peak_mem = 0
        cpu_usec = 0
        with (out / 'stdout.log').open('w') as stdout, (out / 'stderr.log').open('w') as stderr:
            proc = subprocess.Popen(args, cwd=ROOT, stdout=stdout, stderr=stderr)
            cgroup = None
            while proc.poll() is None:
                if cgroup is None:
                    result = subprocess.run(['systemctl', '--user', 'show', unit, '--property=ControlGroup', '--value'],
                                            capture_output=True, text=True)
                    if result.stdout.strip():
                        cgroup = Path('/sys/fs/cgroup') / result.stdout.strip().lstrip('/')
                if cgroup:
                    try:
                        peak_mem = max(peak_mem, int((cgroup / 'memory.current').read_text()))
                        stats = dict(line.split() for line in (cgroup / 'cpu.stat').read_text().splitlines())
                        cpu_usec = max(cpu_usec, int(stats['usage_usec']))
                    except (OSError, KeyError):
                        pass
                # Do not wait for the disk to fill; stop only our own service.
                if shutil.disk_usage(ROOT).free < a.reserve_disk_gb * 2**30:
                    rec['resource_stop'] = 'disk_reserve'
                    subprocess.run(['systemctl', '--user', 'stop', unit], capture_output=True)
                if available_mb() < a.reserve_mb:
                    rec['resource_stop'] = 'system_memory_reserve'
                    subprocess.run(['systemctl', '--user', 'stop', unit], capture_output=True)
                if time.monotonic() - t0 > a.timeout + 100:
                    rec['resource_stop'] = 'runner_deadline'
                    subprocess.run(['systemctl', '--user', 'stop', unit], capture_output=True)
                    proc.terminate()
                time.sleep(.25)
        code = proc.wait(timeout=10)
        rec.update(exit_code=code, status='completed' if code == 0 else 'failed',
                   peak_cgroup_sampled_mb=peak_mem / 2**20, cgroup_cpu_s=cpu_usec / 1e6)
        if marker.exists():
            rec['worker_result'] = json.loads(marker.read_text())
            if rec['worker_result']['forwarded_signal']:
                rec['status'] = 'interrupted'
            elif rec['worker_result']['exit_code'] != 0:
                rec['status'] = 'failed'
        elif rec['status'] == 'completed':
            rec['status'] = 'missing_completion_marker'
        rec['service_properties'] = subprocess.run(
            ['systemctl', '--user', 'show', unit, '-p', 'Result', '-p', 'MemoryPeak', '-p', 'CPUUsageNSec'],
            capture_output=True, text=True).stdout
        if rec['status'] == 'completed':
            rec['outputs'] = {str(Path(f).resolve()): digest(f) for f in a.output}
        if rec.get('resource_stop'):
            rec['status'] = 'resource_stopped'
    except BaseException as e:
        rec.update(status='infrastructure_failed', error=f'{type(e).__name__}: {e}')
        if proc is not None and proc.poll() is None:
            subprocess.run(['systemctl', '--user', 'stop', unit], capture_output=True)
            proc.wait(timeout=15)
        if isinstance(e, KeyboardInterrupt):
            rec['status'] = 'interrupted'
    finally:
        if rec.get('source_patch_sha256'):
            current_patch = subprocess.check_output(['git', 'diff', '--binary', 'HEAD'], cwd=ROOT)
            rec['tracked_source_unchanged_during_run'] = hashlib.sha256(current_patch).hexdigest() == rec['source_patch_sha256']
            if rec['status'] == 'completed' and not rec['tracked_source_unchanged_during_run']:
                rec['status'] = 'source_changed_during_run'
        rec['wall_s'] = time.monotonic() - t0
        save()
        print(json.dumps({'status': rec['status'], 'record': str(status_file), 'wall_s': rec['wall_s']}))
    return 0 if rec['status'] == 'completed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
