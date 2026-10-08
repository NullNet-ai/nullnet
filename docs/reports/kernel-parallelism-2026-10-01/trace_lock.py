import gzip
import json
import os
from pathlib import Path
import time

ROOT = Path('/sys/kernel/tracing')
GROUP = 'nn_est'
INSTANCE = ROOT / 'instances' / GROUP
OUT = Path('/tmp/nn-kernel-estimate')
LOCKS = ['rtnl_lock', 'rtnl_lock_killable', 'rtnl_trylock']
FUNCTIONS = ['rtnl_newlink', 'rtnl_setlink', 'rtnl_dellink',
             'register_netdevice', 'unregister_netdevice_many',
             'synchronize_rcu_expedited', 'synchronize_net', 'rcu_barrier',
             'synchronize_rcu', 'flush_all_backlogs', 'vxlan_sock_release', 'vxlan_flush', 'udp_tunnel_sock_release', 'dev_deactivate_many', 'dev_close_many', 'call_netdevice_notifiers', 'netdev_unregister_kobject', 'dev_shutdown', 'napi_disable', 'dev_change_name', 'br_add_if', 'br_del_if', 'vxlan_open', 'vxlan_stop',
             'inet_rtm_newaddr', 'inet_rtm_deladdr', 'netdev_run_todo']

def write(path, value):
    path.write_text(str(value))

def trace_start(label, pid):
    if INSTANCE.exists():
        assert (INSTANCE / 'tracing_on').read_text().strip() == '0'
        assert (INSTANCE / 'current_tracer').read_text().strip() == 'nop'
        assert GROUP + '/' not in (ROOT / 'kprobe_events').read_text()
        INSTANCE.rmdir()
    available = {line.split()[0] for line in (ROOT / 'available_filter_functions').read_text().splitlines()}
    functions = [f for f in FUNCTIONS if f in available]
    definitions = []
    for f in LOCKS + functions:
        definitions.extend([f'p:{GROUP}/{f}_in {f}',
                            f'r128:{GROUP}/{f}_out {f}' + (' retval=$retval:s64' if f in LOCKS[1:] else '')])
    definitions.append(f'p:{GROUP}/release __rtnl_unlock')
    definitions.extend([f'p:{GROUP}/mutex_in mutex_lock lock=$arg1:x64', f'r2048:{GROUP}/mutex_out mutex_lock'])
    addresses = [line.split()[0] for line in Path('/proc/kallsyms').read_text().splitlines() if line.split()[-1] == 'rtnl_mutex']
    assert len(addresses) == 1 and int(addresses[0],16) != 0
    mutex_address = '0x' + addresses[0]
    INSTANCE.mkdir()
    write(INSTANCE / 'tracing_on', 0)
    write(INSTANCE / 'trace_clock', 'mono')
    write(INSTANCE / 'buffer_size_kb', 32768)
    descriptor = os.open(ROOT / 'kprobe_events', os.O_WRONLY | os.O_APPEND)
    try:
        for definition in definitions:
            os.write(descriptor, (definition + '\n').encode())
    finally:
        os.close(descriptor)
    write(INSTANCE / 'events' / GROUP / 'mutex_in' / 'filter', 'lock == ' + mutex_address)
    write(INSTANCE / 'events' / GROUP / 'enable', 1)
    write(INSTANCE / 'events/sched/sched_switch/filter', 'prev_comm == \"nn-est-worker\" || prev_comm == \"nn-est-main\" || next_comm == \"nn-est-worker\" || next_comm == \"nn-est-main\"')
    write(INSTANCE / 'events/sched/sched_switch/trigger', 'stacktrace if prev_comm == \"nn-est-worker\" || prev_comm == \"nn-est-main\"')
    write(INSTANCE / 'events/sched/sched_switch/enable', 1)
    write(INSTANCE / 'trace', '')
    meta = {'label': label, 'pid': pid, 'functions': functions,
            'events': [line.split()[0].split('/', 1)[1] for line in definitions],
            'mutex_address':mutex_address, 'start': time.monotonic()}
    (OUT / 'trace-meta.json').write_text(json.dumps(meta))
    write(INSTANCE / 'tracing_on', 1)
    return {'tracing': True}

def trace_stop(label, pid):
    write(INSTANCE / 'tracing_on', 0)
    meta = json.loads((OUT / 'trace-meta.json').read_text())
    meta['stop'] = time.monotonic()
    meta['tids'] = [int(p.name) for p in Path(f'/proc/{pid}/task').iterdir()]
    meta['stats'] = {p.parent.name: p.read_text() for p in (INSTANCE / 'per_cpu').glob('cpu*/stats')}
    meta['kprobe_profile'] = (ROOT / 'kprobe_profile').read_text()
    with gzip.open(OUT / (meta['label'] + '.trace.gz'), 'wt') as stream:
        stream.write((INSTANCE / 'trace').read_text())
    (OUT / (meta['label'] + '.trace-meta.json')).write_text(json.dumps(meta, indent=2))
    write(INSTANCE / 'events' / GROUP / 'enable', 0)
    write(INSTANCE / 'events/sched/sched_switch/enable', 0)
    write(INSTANCE / 'events/sched/sched_switch/trigger', '!stacktrace')
    INSTANCE.rmdir()
    descriptor = os.open(ROOT / 'kprobe_events', os.O_WRONLY | os.O_APPEND)
    try:
        for event in meta['events']:
            os.write(descriptor, f'-:{GROUP}/{event}\n'.encode())
    finally:
        os.close(descriptor)
    return {'tracing': False, 'seconds': meta['stop'] - meta['start'], 'stats': meta['stats']}
