"""Open-loop client arrivals with bounded measurement memory and mixed session lengths."""
import argparse, asyncio, importlib.util, ipaddress, json, pathlib, random, signal, ssl, time

spec=importlib.util.spec_from_file_location('http_load',pathlib.Path(__file__).with_name('http-load.py'))
transport=importlib.util.module_from_spec(spec);spec.loader.exec_module(transport)
p=argparse.ArgumentParser();p.add_argument('--rate',type=float,required=True);p.add_argument('--duration',type=float,required=True)
p.add_argument('--output',required=True);p.add_argument('--stop-file',required=True)
p.add_argument('--diagnostic',action='store_true');p.add_argument('--tls-ca');p.add_argument('--payload-every',type=int,default=100)
p.add_argument('--services',default='1,7,2,8,3,9,4,10,5,11,6,12');p.add_argument('--identities',type=int,default=65520);p.add_argument('--seed',type=int,default=20261009)
a=p.parse_args();assert a.rate>0 and a.duration>0 and a.identities%12==0
assert a.identities/a.rate>145, 'Address reuse must exceed session duration plus request timeout and idle grace'
context=ssl.create_default_context(cafile=a.tls_ca) if a.tls_ca else None
if context:context.set_alpn_protocols(['http/1.1'])
services=[int(x) for x in a.services.split(',')];assert len(services)==12
base=int(ipaddress.IPv4Address('198.19.0.1'));output=pathlib.Path(a.output);assert not output.exists()
stop=asyncio.Event();active=set();occupied=set();inflight=0;latencies=[];cold=[];warm=[]
counters={'clients_started':0,'clients_finished':0,'clients_cancelled':0,'requests':0,'errors':0,'missed_requests':0,'bytes':0,'connections_opened':0,'connections_reused':0,'payload_requests':0,'single':0,'medium':0,'long':0,'latency_exceedances':0}
reason=None;first_error=None;max_active=0;max_inflight=0;max_scheduler_lag=0

def fail(message):
 global reason
 if reason is None:reason=message
 stop.set()

def percentiles(values):
 values.sort()
 return {key:(values[min(len(values)-1,int(len(values)*q))]*1000 if values else None) for key,q in [('p50_ms',.5),('p95_ms',.95),('p99_ms',.99)]}

async def client(n,requests):
 global inflight,max_inflight,first_error
 identity=n%a.identities;occupied.add(identity);source=str(ipaddress.IPv4Address(base+identity));service=services[n%len(services)];start=time.monotonic();session=[None,None]
 try:
  for i in range(requests):
   due=start+i*.5;delay=due-time.monotonic()
   if delay>0:await asyncio.sleep(delay)
   elif i and delay<-.5:
    counters['missed_requests']+=1
    continue
   if inflight>=2048:fail('2048 concurrent requests reached');return
   inflight+=1;max_inflight=max(max_inflight,inflight)
   try:
    path='/payload.bin' if a.payload_every and (n*257+i)%a.payload_every==0 else '/'
    r=await transport.http_request(n,source,service,path=path,connect_timeout=10,response_timeout=20,session=session,tls_context=context)
   finally:inflight-=1
   counters['requests']+=1;counters['payload_requests']+=int(path=='/payload.bin');counters['connections_reused' if r.get('connection_reused') else 'connections_opened']+=1;counters['bytes']+=r.get('bytes',0);latencies.append(r['seconds']);(cold if i==0 else warm).append(r['seconds'])
   if not r['ok']:
    counters['errors']+=1;first_error=first_error or r;fail('HTTP request failed');return
   if r['seconds']>5:
    counters['latency_exceedances']+=1
    if not a.diagnostic:fail('HTTP response latency exceeds five seconds');return
   if r['seconds']>20:fail('HTTP response latency exceeds twenty-second safety limit');return
  counters['clients_finished']+=1
 except asyncio.CancelledError:
  counters['clients_cancelled']+=1;raise
 finally:
  occupied.remove(identity)
  if session[1] is not None:
   session[1].close()
   try:await asyncio.wait_for(session[1].wait_closed(),5)
   except (ConnectionError,OSError,asyncio.TimeoutError):pass

async def monitor(start):
 previous=dict(counters);last=start
 with output.open('x') as stream:
  while True:
   now=time.monotonic();span=now-last
   row={'unix':time.time(),'elapsed':now-start,'rate_target':a.rate,'active_clients':len(active),'inflight_requests':inflight,'max_active_clients':max_active,'max_inflight_requests':max_inflight,'max_scheduler_lag_ms':max_scheduler_lag*1000,'counters':dict(counters),'stop_reason':reason,'first_error':first_error,'latency':percentiles(latencies),'cold_latency':percentiles(cold),'warm_latency':percentiles(warm),'window_seconds':span,'arrivals_per_second':(counters['clients_started']-previous['clients_started'])/span if span else 0,'requests_per_second':(counters['requests']-previous['requests'])/span if span else 0}
   stream.write(json.dumps(row)+'\n');stream.flush();tmp=output.with_suffix('.status.tmp');tmp.write_text(json.dumps(row));tmp.replace(output.with_suffix('.status.json'))
   latencies.clear();cold.clear();warm.clear();previous=dict(counters);last=now
   if pathlib.Path(a.stop_file).exists():fail('observer stop requested')
   await asyncio.sleep(5)

async def main():
 global max_active,max_scheduler_lag
 start=time.monotonic();rng=random.Random(a.seed);observer=asyncio.create_task(monitor(start));loop=asyncio.get_running_loop()
 for sig in [signal.SIGTERM,signal.SIGINT]:loop.add_signal_handler(sig,lambda:fail('signal stop requested'))
 due=start;n=0
 try:
  while due-start<a.duration and not stop.is_set():
   delay=due-time.monotonic()
   if delay>0:
    try:await asyncio.wait_for(stop.wait(),delay);break
    except asyncio.TimeoutError:pass
   lag=time.monotonic()-due;max_scheduler_lag=max(max_scheduler_lag,lag)
   if lag>1:fail('arrival scheduler lag exceeds one second');break
   if len(active)>=4096:fail('4096 live clients reached');break
   if n%a.identities in occupied:fail('source identity still active at scheduled reuse');break
   kind=rng.random();category,requests=('single',1) if kind<.75 else ('medium',20) if kind<.95 else ('long',240)
   counters[category]+=1;counters['clients_started']+=1
   task=asyncio.create_task(client(n,requests));active.add(task);task.add_done_callback(active.discard);max_active=max(max_active,len(active))
   n+=1;due+=rng.expovariate(a.rate)
  if stop.is_set():
   for task in list(active):task.cancel()
  await asyncio.gather(*list(active),return_exceptions=True)
  await asyncio.sleep(5)
 finally:
  observer.cancel();await asyncio.gather(observer,return_exceptions=True)
  summary={'finished':True,'unix_end':time.time(),'elapsed':time.monotonic()-start,'arrival_duration':a.duration,'target_arrivals_per_second':a.rate,'counters':counters,'stop_reason':reason,'first_error':first_error,'max_active_clients':max_active,'max_inflight_requests':max_inflight,'max_scheduler_lag_ms':max_scheduler_lag*1000}
  output.with_suffix('.summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary),flush=True)
 if reason:raise SystemExit(1)

asyncio.run(main())
