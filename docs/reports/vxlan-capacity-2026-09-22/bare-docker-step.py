import subprocess,time,json,concurrent.futures,threading,pathlib
ids=subprocess.check_output(['docker','ps','-q'],text=True).split();gate=threading.Semaphore(8)
def one(i):
 with gate:
  p=subprocess.check_output(['docker','inspect','-f','{{.State.Pid}}',ids[i%len(ids)]],text=True)
  assert int(p)>0
start=time.monotonic()
with concurrent.futures.ThreadPoolExecutor(max_workers=32) as ex:list(ex.map(one,range(1000)))
elapsed=time.monotonic()-start
r={'step':'Docker PID lookup (container alternative)','endpoints':1000,'operations_per_endpoint':1,'seconds':elapsed,'endpoints_per_second':1000/elapsed,'operations_per_second':1000/elapsed,'errors':[]}
pathlib.Path('/tmp/nn-docker-step.json').write_text(json.dumps(r));print(json.dumps(r),flush=True)
