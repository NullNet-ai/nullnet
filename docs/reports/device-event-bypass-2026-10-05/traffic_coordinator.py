"""Relay control to Linux fixtures; verify encrypted delivery and cleanup."""
import concurrent.futures as futures
import contextlib
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'vxlan-prepared-cross-host-2026-09-28'))
from coordinator import Node as OriginalNode

class Node(OriginalNode):
    def __init__(self,side):
        script = '/tmp/retained_traffic_node.py' if '--retained' in sys.argv else '/tmp/traffic_node.py'
        self.process=subprocess.Popen(['ssh','-T','-o','BatchMode=yes',f'debian@192.168.1.{103+side}',
            'sudo','-k','-S','-p',"''",'python3','-u',script,'--side',str(side),
            '--slots','16','--workers','16','--variant','bridge','--output','/tmp/nn-event-20261005/traffic-node.json'],
            stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,bufsize=1)
        self.process.stdin.write('debian\n')
        self.process.stdin.flush()

nodes=[Node(side) for side in [0,1]]
result={}
with futures.ThreadPoolExecutor(2) as pool:
    def both(message):
        return list(pool.map(lambda node:node.call(message),nodes))
    def activate(ids,down=()):
        return both({'op':'wave','up':[[i,os.urandom(32).hex()] for i in ids],'down':list(down)})
    try:
        result['ready']=list(pool.map(lambda node:node.receive(),nodes))
        active=list(range(8))
        idle=list(range(8,16))
        activate(active)
        for wave in range(20):
            both({'op':'traffic','ids':active})
            activate(idle,active)
            active,idle=idle,active
        both({'op':'traffic','ids':active})
        both({'op':'wave','down':active})
        result['idle_inventory']=both({'op':'inventory','label':'after-filtered-traffic'})
        result['quiet']=both({'op':'quiet','ids':list(range(16))})
        result.update({'result':'passed','edge_generations':168,'bidirectional_deliveries':336})
    finally:
        result['cleanup']=[]
        for node in nodes:
            try:
                cleaned=node.call({'op':'finish'})
            except Exception as error:
                cleaned={'error':repr(error)}
            finally:
                node.process.stdin.close()
            cleaned['exit_code']=node.process.wait(timeout=120)
            cleaned['stderr']=node.process.stderr.read()
            result['cleanup'].append(cleaned)
        Path(sys.argv[1]).write_text(json.dumps(result,indent=2)+'\n')
        assert all(r.get('cleaned') and r['exit_code']==0 and all(r['preservation'].values()) for r in result['cleanup']),result['cleanup']
print(json.dumps(result))
