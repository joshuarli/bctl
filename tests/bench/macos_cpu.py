"""macOS process CPU snapshots, including CPU charged for reaped children."""

import ctypes
import os


class Timebase(ctypes.Structure):
    _fields_ = [('numer', ctypes.c_uint32), ('denom', ctypes.c_uint32)]


class Usage(ctypes.Structure):
    _fields_ = [('uuid', ctypes.c_ubyte * 16), *[(name, ctypes.c_uint64) for name in (
        'user', 'system', 'idle_wakeups', 'interrupt_wakeups', 'pageins', 'wired', 'resident', 'footprint',
        'start', 'exit', 'child_user', 'child_system', 'child_idle_wakeups', 'child_interrupt_wakeups',
        'child_pageins', 'child_elapsed')]]


def snapshot(roots):
    system = ctypes.CDLL('/usr/lib/libSystem.B.dylib')
    timebase = Timebase()
    if system.mach_timebase_info(ctypes.byref(timebase)) or not timebase.denom:
        raise RuntimeError('mach_timebase_info failed')
    lib = ctypes.CDLL('/usr/lib/libproc.dylib', use_errno=True)
    lib.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
    lib.proc_listchildpids.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_int]
    owned = set()
    pending = list(roots)
    processes = {}
    while pending:
        pid = pending.pop()
        if pid in owned:
            continue
        owned.add(pid)
        capacity = 64
        while True:
            children = (ctypes.c_int * capacity)()
            count = lib.proc_listchildpids(pid, children, ctypes.sizeof(children))
            if count < 0:
                raise OSError(ctypes.get_errno(), f'proc_listchildpids({pid})')
            if count < capacity:
                break
            capacity *= 2
        pending.extend(child for child in children[:count] if child)
        usage = Usage()
        if lib.proc_pid_rusage(pid, 1, ctypes.byref(usage)):
            raise OSError(ctypes.get_errno(), f'proc_pid_rusage({pid})')
        processes[pid] = {key: getattr(usage, key) for key in ('start', 'user', 'system', 'child_user', 'child_system')}
    return {'processes': processes, 'timebase': {'numer': timebase.numer, 'denom': timebase.denom},
            'cpu_s': sum(row[key] for row in processes.values()
                for key in ('user', 'system', 'child_user', 'child_system')) * timebase.numer / timebase.denom / 1e9}


def trial_snapshot(trial):
    from macos_workflows import process_table

    trial.discover_daemon()
    roots = {os.getpid(), *trial.roots}
    for line in process_table().splitlines():
        pid, _, command = line.strip().split(None, 2)
        if str(trial.path) in command:
            roots.add(int(pid))
    for pidfile in trial.socket_dir.rglob('*.pid'):
        roots.add(int(pidfile.read_text().strip()))
    return snapshot(roots)
