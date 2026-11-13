from pathlib import Path
import os
import signal
import subprocess
import tempfile
import time


ROOT = Path(__file__).resolve().parent.parent
WORK = ROOT / '.work'


def run(cpus):
    share = Path(tempfile.mkdtemp(prefix=f'p9-mutation-smp{cpus}-', dir=WORK / 'tmp'))
    for i in range(128):
        (share / f'distinct-{i}').write_text('d')
    (share / 'victim').write_text('original')
    log = share / 'runner.log'
    with log.open('w') as out:
        runner = subprocess.Popen([str(ROOT / 'dev/run.sh'), '--smp', str(cpus), '--timeout', '40',
            '--9p', f'1:{share}', '--label', f'p9-mutation-smp{cpus}',
            str(WORK / 'images/p9-mutation.img')], stdout=out, stderr=out, start_new_session=True)
    try:
        deadline = time.monotonic() + 10
        serial = None
        while time.monotonic() < deadline and runner.poll() is None:
            candidates = list((WORK / 'runs').glob(f'*-p9-mutation-smp{cpus}-{runner.pid}/serial.log'))
            if candidates and 'p9-mutation: closed' in candidates[0].read_text():
                serial = candidates[0]
                break
            time.sleep(.01)
        if serial is None:
            raise RuntimeError(f'guest did not reach closed phase: {log}')
        children = Path(f'/proc/{runner.pid}/task/{runner.pid}/children').read_text().split()
        qemu, = [pid for pid in children if Path(f'/proc/{pid}/comm').read_text().startswith('qemu')]
        deadline = time.monotonic() + 5
        retained = []
        while time.monotonic() < deadline:
            retained = []
            for fd in Path(f'/proc/{qemu}/fd').iterdir():
                try:
                    target = str(fd.readlink())
                    if target.startswith(str(share / 'distinct-')):
                        retained.append(target)
                except FileNotFoundError:
                    pass
            if not retained:
                break
            time.sleep(.01)
        print(f'{"FAIL" if retained else "PASS"} p9 closed server descriptors smp={cpus} retained={len(retained)}', flush=True)
        (share / 'closed-ack').touch()
        deadline = time.monotonic() + 10
        while 'p9-mutation: open' not in serial.read_text():
            if runner.poll() is not None or time.monotonic() >= deadline:
                raise RuntimeError(f'guest did not reach open phase: {serial}')
            time.sleep(.01)
        (share / 'victim').unlink()
        (share / 'open-ack').touch()
        status = runner.wait(timeout=40)
        print(log.read_text(), flush=True)
        if status or retained:
            raise RuntimeError(f'9p ownership failed: {share}')
    finally:
        if runner.poll() is None:
            os.killpg(runner.pid, signal.SIGTERM)
            try:
                runner.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(runner.pid, signal.SIGKILL)
                runner.wait()


if __name__ == '__main__':
    def interrupted(signum, frame):
        raise SystemExit(128 + signum)
    for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(signum, interrupted)
    for cpus in (1, 2):
        run(cpus)
