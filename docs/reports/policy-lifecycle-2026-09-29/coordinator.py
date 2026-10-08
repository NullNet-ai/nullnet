"""Two-host policy lifecycle benchmark; SSH coordination is timed."""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import statistics
import subprocess
import time

class Node:
    def __init__(self,side,folder,stress=False):
        self.log=open(folder/f'stderr-{side}.log','w')
        self.process=subprocess.Popen(['ssh','-T','-o','BatchMode=yes',f'debian@192.168.1.{103+side}',
            'sudo','-k','-S','-p',"''",'python3','-u','/tmp/nn29-policy/policy-lifecycle-2026-09-29/'+('stress_node.py' if stress else 'node.py'),'--side',str(side)],
            stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=self.log,text=True,bufsize=1)
        self.process.stdin.write('debian\n');self.process.stdin.flush()
    def send(self,message):self.process.stdin.write(json.dumps(message)+'\n');self.process.stdin.flush()
    def receive(self):
        line=self.process.stdout.readline()
        assert line,('node exited',self.process.poll())
        data=json.loads(line)
        assert 'error' not in data,data
        return data
    def call(self,message):self.send(message);return self.receive()


def main():
    p=argparse.ArgumentParser();p.add_argument('--output',required=True);p.add_argument('--seconds',type=float,default=60);p.add_argument('--trials',type=int,default=3);p.add_argument('--mode',choices=['burst','overlap'],default='overlap');p.add_argument('--stress',action='store_true');args=p.parse_args()
    output=Path(args.output);output.parent.mkdir(parents=True,exist_ok=True)
    result={'stress':args.stress,'mode':args.mode,'scope':'isolated cross-host kernel prototype','trials':[],'proofs':[],'completed':False,'controller_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}
    nodes=[Node(i,output.parent,args.stress) for i in [0,1]]
    pool=concurrent.futures.ThreadPoolExecutor(2)
    def both(msg):return list(pool.map(lambda n:n.call(msg),nodes))
    def save():output.write_text(json.dumps(result,indent=2)+'\n')
    def proof(name,value):result['proofs'].append({'name':name,'result':value});print('PASS',name,flush=True);save()
    ids=list(range(64));generation=1
    def activate(gen,lease=2000,selected=ids):
        prep=both({'op':'prepare','generation':gen,'lease_ms':lease,'ids':selected})
        commit=both({'op':'commit','generation':gen,'lease_ms':lease,'ids':selected})
        return prep,commit
    def negative_injection(name,gen,reason,**kwargs):
        before=nodes[1].call({'op':'counters'})
        sent=nodes[0].call({'op':'inject','generation':gen,**kwargs})
        quiet=nodes[1].call({'op':'quiet','wait':.05})
        assert quiet['counters'][reason] >= before[reason]+1,(name,before,quiet,sent)
        proof(name,{'sent':sent,'receiver':quiet})
    try:
        key=os.urandom(32).hex()
        for node in nodes:node.send({'key':key})
        result['ready']=list(pool.map(lambda n:n.receive(),nodes));save()
        proof('default-deny',both({'op':'denied'}));both({'op':'quiet'})
        nodes[0].call({'op':'prepare','generation':generation})
        proof('partial-prepare-denied',both({'op':'denied'}));both({'op':'retire'})
        activate(generation)
        nodes[0].call({'op':'capture'})
        proof('encrypted-bidirectional-traffic',both({'op':'traffic','label':1}))
        time.sleep(.1)
        proof('ciphertext-only-underlay',nodes[0].call({'op':'capture-stop'}))
        replay_before=nodes[1].call({'op':'xfrm-counters'})
        replay=nodes[0].call({'op':'replay'})
        replay_quiet=nodes[1].call({'op':'quiet','wait':.05})
        replay_after=nodes[1].call({'op':'xfrm-counters'})
        assert replay_after['XfrmInStateSeqError']-replay_before['XfrmInStateSeqError']>=replay['replayed']
        proof('encrypted-packet-replay-denied',{'replay':replay,'receiver':replay_quiet})
        for fault in ['missing','wrong-key']:
            nodes[1].call({'op':'fault','mode':fault})
            nodes[0].call({'op':'send'})
            proof('crypto-'+fault+'-denied',nodes[1].call({'op':'quiet','wait':.05}))
            nodes[1].call({'op':'fault','mode':'restore'})
            proof('crypto-'+fault+'-restored',both({'op':'traffic','label':3}))
        before=nodes[0].call({'op':'counters'})
        nodes[0].call({'op':'spoof','endpoint':1,'id':0})
        quiet=nodes[1].call({'op':'quiet','wait':.05});after=nodes[0].call({'op':'counters'})
        assert after[1]>before[1],(before,after)
        proof('source-identity-spoof-denied',{'receiver':quiet,'sender_counters':after})
        negative_injection('wrong-generation-denied',generation+1,8)
        negative_injection('unknown-source-denied',generation,7,source='10.230.0.99')
        nodes[1].call({'op':'retire'})
        negative_injection('receiver-revocation-denied',generation,7)
        nodes[0].call({'op':'retire'})
        proof('both-retired-denied',both({'op':'denied'}))
        generation+=1;activate(generation)
        negative_injection('previous-generation-after-reuse-denied',generation-1,8)
        proof('new-generation-delivered',both({'op':'traffic','label':2}))
        both({'op':'retire'})
        generation+=1
        proof('lease-writer-process-exited',both({'op':'writer-exit','generation':generation,'lease_ms':100}))
        time.sleep(.2)
        proof('controller-renewal-loss-expires',both({'op':'denied'}))
        negative_injection('expired-receiver-denied',generation,7)
        both({'op':'retire'});both({'op':'quiet'})
        if args.stress:
            both({'op':'tcp-open'})
            proof('tcp-established-bidirectional',both({'op':'tcp-connect'}))
            both({'op':'tcp-revoke'});both({'op':'tcp-send-retired'})
            proof('tcp-established-revoked',both({'op':'tcp-quiet'}))
            both({'op':'tcp-close'})
        result['initial_inventory']=both({'op':'inventory'})
        if args.stress:
            nodes[1].call({'op':'background-start'})
            nodes[0].call({'op':'background-start'})
        for trial in range(args.trials):
            cohorts=[list(range(32)),list(range(32,64))]
            if args.mode=='overlap':generation+=1;activate(generation,selected=cohorts[1])
            start=time.monotonic();cycles=0;turns=0;phases=[]
            while time.monotonic()-start<args.seconds or turns<20:
                up=ids if args.mode=='burst' else cohorts[turns%2]
                down=ids if args.mode=='burst' else cohorts[1-turns%2]
                generation+=1;t=time.monotonic()
                prep,commit=activate(generation,selected=up)
                t1=time.monotonic();both({'op':'traffic','label':generation});t2=time.monotonic()
                retired=both({'op':'retire','ids':down});t3=time.monotonic()
                denied=both({'op':'denied','ids':down});both({'op':'quiet'});t4=time.monotonic()
                assert all(x['denied']==len(down) for x in denied)
                phases.append({'activation_s':t1-t,'traffic_s':t2-t1,'retirement_s':t3-t2,'rejection_s':t4-t3,
                               'prepare':prep,'commit':commit,'retire':retired})
                cycles+=len(up);turns+=1
            elapsed=time.monotonic()-start
            drain_start=time.monotonic();both({'op':'retire'});both({'op':'denied'});both({'op':'quiet'})
            row={'trial':trial,'seconds':elapsed,'creations':cycles,'retirements':cycles,'rejection_checks':cycles*2,'cycles_per_second':cycles/elapsed,'batches':turns,'final_drain_seconds':time.monotonic()-drain_start,'phases':phases}
            result['trials'].append(row);save();print('TRIAL',trial,cycles/elapsed,flush=True)
        if args.stress:
            bg=[nodes[0].call({'op':'background-stop'}),nodes[1].call({'op':'background-stop'})]
            assert all(not item['errors'] and item['exchanges']>0 for item in bg),bg
            assert bg[0]['exchanges']==bg[1]['exchanges'],bg
            proof('independent-flow-survives-churn',bg)
        result['final_inventory']=both({'op':'inventory'})
        assert result['initial_inventory']==result['final_inventory']
        generation+=1;activate(generation,lease=5000)
        warm_start=time.monotonic();warm_pairs=0
        while time.monotonic()-warm_start<1:
            both({'op':'traffic','label':generation});warm_pairs+=64
        warm_seconds=time.monotonic()-warm_start
        result['warm']={'bidirectional_exchanges':warm_pairs,'seconds':warm_seconds,'exchanges_per_second':warm_pairs/warm_seconds}
        both({'op':'retire'});both({'op':'denied'});both({'op':'quiet'})
        result['final_counters']=both({'op':'counters'})
        result['final_quiet']=both({'op':'quiet','wait':.1})
        result['completed']=True
    except Exception as e:
        result['error']=repr(e);raise
    finally:
        result['cleanup']=[]
        for node in nodes:
            try:result['cleanup'].append(node.call({'op':'finish'}))
            except Exception as e:result['cleanup'].append({'error':repr(e)})
        for node in nodes:
            node.process.stdin.close()
            try:node.process.wait(timeout=30)
            except subprocess.TimeoutExpired:node.process.terminate();result['completed']=False
            node.log.close()
        pool.shutdown()
        if not all(c.get('cleaned') for c in result['cleanup']):result['completed']=False
        save()
    assert result['completed'],result.get('cleanup')
    if result['trials']:print('MEDIAN',statistics.median(t['cycles_per_second'] for t in result['trials']))

if __name__=='__main__':main()
