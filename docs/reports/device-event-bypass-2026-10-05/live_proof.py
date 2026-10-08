"""Verify scoped notification delivery and foreign-device behavior on Linux."""
import json
import os
from pathlib import Path
import runpy
import select
import signal
import socket
import struct
import subprocess
import sys
import time

sys.path.insert(0,'/tmp/nn-cpu-20261005/docs/reports/kernel-parallelism-2026-10-01')
import node
from socket_filter import Filters

ROOT=Path('/tmp/nn-event-20261005')
before=node.original_inventory()
runpy.run_path('/tmp/nn-event-discover.py')
filters=Filters(json.loads(Path('/tmp/nn-event-sockets.json').read_text()),ROOT/'proof-filters.json')
sockets={}
created=set()
owned='nnv_1979999_s'
foreign='nfctrl0'
replacement='nfctrl1'
records=[]
def interrupted(signum,frame):
    raise KeyboardInterrupt
signal.signal(signal.SIGTERM,interrupted)


def collect(label):
    deadline=time.monotonic()+2
    while time.monotonic()<deadline:
        ready,_,_=select.select(list(sockets.values()),[],[],.1)
        for s in ready:
            data=s.recv(65536)
            name=next(k for k,v in sockets.items() if v is s)
            if name.startswith('uevent'):
                names=[part[10:].decode() for part in data.split(b'\0') if part.startswith(b'INTERFACE=')]
                if names:
                    records.append({'phase':label,'socket':name,'names':names})
            else:
                offset=0
                while offset+16<=len(data):
                    size,kind,flags,seq,pid=struct.unpack_from('IHHII',data,offset)
                    if kind in (16,17):
                        pos=offset+32
                        names=[]
                        while pos+4<=offset+size:
                            length,typ=struct.unpack_from('HH',data,pos)
                            if typ&16383==3:
                                names.append(data[pos+4:pos+length].rstrip(b'\0').decode())
                            pos+=(length+3)&~3
                        records.append({'phase':label,'socket':name,'names':names,'type':kind,'seq':seq})
                    elif kind in (20,21,28,29):
                        index=struct.unpack_from('I',data,offset+20)[0]
                        records.append({'phase':label,'socket':name,'names':[],'type':kind,'index':index})
                    offset+=(size+3)&~3


try:
    for protocol,prefix,groups in [(15,'uevent',1),(0,'route',0x555)]:
        for suffix in ['unfiltered','filtered']:
            s=socket.socket(socket.AF_NETLINK,socket.SOCK_RAW,protocol)
            s.bind((0,groups))
            s.setblocking(False)
            sockets[prefix+'-'+suffix]=s
            if suffix=='filtered':
                if protocol==15:
                    program=filters.programs['uevent']
                else:
                    from socket_filter import route_program
                    program=route_program(filters.owned,filters.counters,s.getsockname()[0]).load_kernel('nn_evt_proof')
                    filters.programs['proof-route']=program
                s.setsockopt(socket.SOL_SOCKET,50,struct.pack('I',program))
    s=socket.socket(socket.AF_NETLINK,socket.SOCK_RAW,15)
    s.bind((0,2))
    s.setblocking(False)
    sockets['uevent-processed']=s
    filters.apply('all')
    for name in [owned,foreign]:
        node.command('ip','link','add',name,'type','dummy')
        created.add(name)
    index=json.loads(node.command('ip','-j','link','show',owned))[0]['ifindex']
    node.command('ip','addr','add','192.0.2.17/29','dev',owned)
    node.command('ip','link','set',owned,'up')
    node.command('udevadm','settle','--timeout=60')
    collect('created')
    for channel in ['uevent','route']:
        def seen(suffix,name):
            return any(r['phase']=='created' and r['socket']==channel+'-'+suffix and name in r['names'] for r in records)
        assert seen('unfiltered',owned),(channel,'missing unfiltered owned')
        assert not seen('filtered',owned),(channel,'owned was delivered')
        assert seen('filtered',foreign),(channel,'foreign was dropped')
    assert not any(r['socket']=='uevent-processed' and owned in r['names'] for r in records)
    assert any(r['socket']=='uevent-processed' and foreign in r['names'] for r in records)
    assert any(r['phase']=='created' and r['socket']=='route-unfiltered' and r.get('type')==20 and r.get('index')==index for r in records)
    assert not any(r['phase']=='created' and r['socket']=='route-filtered' and r.get('type') in (20,21,28,29) and r.get('index')==index for r in records)
    # A filtered subscriber must still receive its own GETLINK reply.
    s=sockets['route-filtered']
    body=struct.pack('BBHiII',0,0,0,index,0,0)
    s.sendto(struct.pack('IHHII',32,18,1,0x4e1234,s.getsockname()[0])+body,(0,0))
    deadline=time.monotonic()+3
    got_reply=False
    while time.monotonic()<deadline and not got_reply:
        if select.select([s],[],[],.1)[0]:
            data=s.recv(65536)
            got_reply=struct.unpack_from('I',data,8)[0]==0x4e1234
    assert got_reply,'filtered subscriber GETLINK reply missing'
    node.command('ip','link','del',owned)
    created.remove(owned)
    node.command('ip','link','add',replacement,'index',str(index),'type','dummy')
    created.add(replacement)
    node.command('ip','addr','add','192.0.2.25/29','dev',replacement)
    node.command('udevadm','settle','--timeout=60')
    collect('index-reuse')
    assert any(r['phase']=='index-reuse' and r['socket']=='route-filtered' and replacement in r['names'] for r in records)
    assert any(r['phase']=='index-reuse' and r['socket']=='route-filtered' and r.get('type')==20 and r.get('index')==index for r in records)
    (ROOT/'live-proof.json').write_text(json.dumps({'result':'passed','getlink_reply':True,
        'foreign_index_reuse':True,'records':records,'counts':filters.counts()},indent=2)+'\n')
finally:
    filters.apply('baseline')
    for name in list(created):
        node.command('ip','link','del',name)
    filters.close()
    for s in sockets.values():
        s.close()
    after=node.original_inventory()
    checks={key:before[key]==after[key] for key in before}
    (ROOT/'proof-preservation.json').write_text(json.dumps({'checks':checks,'before':before,'after':after},indent=2)+'\n')
    assert all(checks.values()),checks
print(json.dumps({'live_notification_proof':'passed','preservation':checks}))
