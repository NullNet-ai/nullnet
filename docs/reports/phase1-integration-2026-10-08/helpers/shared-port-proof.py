import subprocess,concurrent.futures,shlex,time,json,pathlib
hosts=['103','104']
def root(h,args):return subprocess.check_output(['ssh','debian@192.168.1.'+h,"printf '%s\\n' debian | sudo -S -p '' "+shlex.join(args)],text=True)
for h in hosts:
 root(h,['rm','-f','/tmp/nn-layer1-cross-ready','/tmp/nn-layer1-cross-go','/tmp/nn-layer1-cross-retired','/tmp/nn-layer1-cross-survivor-go'])
procs=[]
for h,side,remote in [('103','s','104'),('104','c','103')]:
 command='NN_PHASE1_TEST_SIDE='+side+' NN_PHASE1_TEST_LOCAL=192.168.1.'+h+' NN_PHASE1_TEST_REMOTE=192.168.1.'+remote+' /tmp/nn-layer1-shared-packet-test cross_host_fresh_endpoint_packet_proof --ignored --nocapture > /tmp/nn-layer1-shared-cross-proof.log 2>&1'
 procs.append(subprocess.Popen(['ssh','debian@192.168.1.'+h,"printf '%s\\n' debian | sudo -S -p '' sh -c "+shlex.quote(command)]))
def wait_file(path):
 end=time.monotonic()+40
 while time.monotonic()<end:
  rows=[root(h,['sh','-c','test -f '+shlex.quote(path)+' && cat '+shlex.quote(path)+' || true']).strip() for h in hosts]
  if all(rows):return rows
  assert not any(p.poll() is not None for p in procs),[root(h,['cat','/tmp/nn-layer1-shared-cross-proof.log']) for h in hosts]
  time.sleep(.2)
 raise RuntimeError(path)
print(wait_file('/tmp/nn-layer1-cross-ready'),flush=True)
listeners=[]
for subnet,id in [(250,1959199),(251,1959200)]:
 root('104',['rm','-f',f'/tmp/nn-layer1-security-ready-{subnet}',f'/tmp/nn-layer1-security-{subnet}.json'])
 cmd=shlex.join(['ip','netns','exec',f'ns_{id}_c','python3','/tmp/shared-port-packets.py','listen',str(subnet)])
 listeners.append(subprocess.Popen(['ssh','debian@192.168.1.104',"printf '%s\\n' debian | sudo -S -p '' "+cmd]))
end=time.monotonic()+5
while True:
 rows=[root('104',['sh','-c',f'test -f /tmp/nn-layer1-security-ready-{subnet} && echo ok || true']).strip() for subnet in [250,251]]
 if all(rows):break
 assert time.monotonic()<end
 time.sleep(.1)
before=root('104',['cat','/proc/net/xfrm_stat'])
print(root('103',['python3','/tmp/shared-port-packets.py','send']))
for p in listeners:assert p.wait(timeout=8)==0
for subnet,label in [(250,'encrypted-A'),(251,'encrypted-B')]:
 rows=json.loads(root('104',['cat',f'/tmp/nn-layer1-security-{subnet}.json']));assert rows==[label],rows
after=root('104',['cat','/proc/net/xfrm_stat'])
stat=lambda raw:dict((k,int(v)) for k,v in (line.split() for line in raw.splitlines()))
a,b=stat(before),stat(after)
assert b['XfrmInStateSeqError']>a['XfrmInStateSeqError'],(a,b)
assert b['XfrmInStateProtoError']>a['XfrmInStateProtoError'],(a,b)
print('PLAINTEXT_WRONG_EDGE_REPLAY_AUTHENTICATION_PASS',flush=True)
for h in hosts:root(h,['touch','/tmp/nn-layer1-cross-go'])
print(wait_file('/tmp/nn-layer1-cross-retired'),flush=True)
for h in hosts:root(h,['touch','/tmp/nn-layer1-cross-survivor-go'])
for p in procs:assert p.wait(timeout=20)==0
for h in hosts:print(root(h,['cat','/tmp/nn-layer1-shared-cross-proof.log']))
