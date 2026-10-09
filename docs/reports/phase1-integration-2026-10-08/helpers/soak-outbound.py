"""Bounded backend/egress traffic in an existing application's network namespace."""
import argparse,http.client,json,pathlib,signal,time
p=argparse.ArgumentParser();p.add_argument('--name',required=True);p.add_argument('--port',type=int,required=True);p.add_argument('--hosts-file');p.add_argument('--expected',required=True);p.add_argument('--rate',type=float,required=True);p.add_argument('--duration',type=float,required=True);a=p.parse_args()
running=True

def stop(*_):
 global running
 running=False
signal.signal(signal.SIGTERM,stop);signal.signal(signal.SIGINT,stop)
start=time.monotonic();next_request=start;next_report=start;connection=None;requests=errors=body_bytes=connections=missed=0
while running and time.monotonic()-start<a.duration:
 now=time.monotonic()
 if now<next_request:time.sleep(min(next_request-now,.1));continue
 next_request+=1/a.rate
 try:
  if connection is None:
   address=a.name
   if a.hosts_file:
    matches=[row.split()[0] for row in pathlib.Path(a.hosts_file).read_text().splitlines() if a.name in row.split()[1:]]
    if not matches:raise RuntimeError('backend hostname missing')
    address=matches[-1]
   connection=http.client.HTTPConnection(address,a.port,timeout=5);connections+=1
  connection.request('GET','/',headers={'Host':a.name,'Connection':'keep-alive'})
  response=connection.getresponse();body=response.read()
  if response.status!=200 or body.decode().strip()!=a.expected:raise RuntimeError('unexpected response '+repr(body[:80]))
  requests+=1;body_bytes+=len(body)
  if response.will_close:connection.close();connection=None
 except Exception as error:
  errors+=1;print(json.dumps({'failure':str(error),'elapsed':time.monotonic()-start}),flush=True)
  if connection:connection.close();connection=None
 if next_request<time.monotonic()-1/a.rate:
  skipped=int((time.monotonic()-next_request)*a.rate);missed+=skipped;next_request+=skipped/a.rate
 if time.monotonic()>=next_report:
  print(json.dumps({'elapsed':time.monotonic()-start,'requests':requests,'errors':errors,'bytes':body_bytes,'connections':connections,'missed':missed}),flush=True);next_report=time.monotonic()+10
if connection:connection.close()
print(json.dumps({'final':True,'requests':requests,'errors':errors,'bytes':body_bytes,'connections':connections,'missed':missed}),flush=True)
