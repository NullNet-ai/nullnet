"""C256 LAN-only cold/warm HTTP workload with explicit client identities."""
import argparse,asyncio,ipaddress,json,time,resource,re,ssl
soft,hard=resource.getrlimit(resource.RLIMIT_NOFILE)
resource.setrlimit(resource.RLIMIT_NOFILE,(min(hard,524288),hard))
p=argparse.ArgumentParser();p.add_argument('--count',type=int,default=1008);p.add_argument('--concurrency',type=int,default=256);p.add_argument('--identities',type=int,default=1008);p.add_argument('--offset',type=int,default=0);p.add_argument('--services',default='1,2,3,4,5,6');p.add_argument('--path',default='/');p.add_argument('--output',required=True);p.add_argument('--tls-ca');
async def http_request(n, source, service, path="/", connect_timeout=90, response_timeout=120, session=None, tls_context=None):
 started=time.monotonic();writer=None;close=True;reused=False
 try:
  if session is not None and session[0] is not None and not session[0].at_eof() and not session[1].is_closing():
   reader,writer=session;reused=True
  else:
   reader,writer=await asyncio.wait_for(asyncio.open_connection('192.168.1.104',443 if tls_context else 80,local_addr=(source,0),ssl=tls_context,server_hostname=f'p1-{service:02d}.test' if tls_context else None),connect_timeout)
   if session is not None:session[:]=[reader,writer]
  connected=time.monotonic()
  request_path='/tree' if path=='/tree-all' else path
  connection='keep-alive' if session is not None else 'close'
  writer.write(f'GET {request_path} HTTP/1.1\r\nHost: p1-{service:02d}.test\r\nConnection: {connection}\r\n\r\n'.encode());await writer.drain()
  header=await asyncio.wait_for(reader.readuntil(b'\r\n\r\n'),response_timeout);headers_received=time.monotonic();status=int(header.split(b' ')[1]);length=None
  for line in header.split(b'\r\n'):
   if line.lower().startswith(b'content-length:'):length=int(line.split(b':',1)[1])
  body=await asyncio.wait_for(reader.readexactly(length) if length is not None else reader.read(),response_timeout)
  close=session is None or b'connection: close' in header.lower() or length is None or (header.startswith(b'HTTP/1.0') and b'connection: keep-alive' not in header.lower())
  good=status==200 and (path!='/tree' or b'node=' in body)
  if path=='/tree-all':good=good and sorted(re.findall(rb'node=(\d+)',body))==[f'{i:02d}'.encode() for i in range(1,13)]
  if path=='/payload.bin':good=good and body==bytes(range(256))*4096
  return {'n':n,'source':source,'service':service,'status':status,'ok':good,'connection_reused':reused,'bytes':len(body),'seconds':time.monotonic()-started,'connect_ms':(connected-started)*1000,'response_headers_ms':(headers_received-connected)*1000,'response_body_ms':(time.monotonic()-headers_received)*1000}
 except Exception as e:return {'n':n,'source':source,'service':service,'ok':False,'error':type(e).__name__+': '+str(e),'seconds':time.monotonic()-started}
 finally:
  if writer and close:
   if session is not None:session[:]=[None,None]
   writer.close()
   try:await asyncio.wait_for(writer.wait_closed(),5)
   except (ConnectionError,OSError,asyncio.TimeoutError):pass

async def request(n):
 async with gate:
  results.append(await http_request(n,str(ipaddress.IPv4Address(base+a.offset+n%a.identities)),services[n%len(services)],a.path,tls_context=tls_context))
async def main():
 start=time.monotonic();unix_start=time.time();await asyncio.gather(*(request(n) for n in range(a.count)));elapsed=time.monotonic()-start;rows=sorted(results,key=lambda r:r['seconds']);ok=sum(r['ok'] for r in rows)
 result={'unix_start':unix_start,'unix_end':time.time(),'count':a.count,'concurrency':a.concurrency,'identities':a.identities,'services':services,'ok':ok,'errors':a.count-ok,'request_seconds':elapsed,'requests_per_second':a.count/elapsed,'p50_ms':rows[len(rows)//2]['seconds']*1000,'p95_ms':rows[int(len(rows)*.95)]['seconds']*1000,'requests':results}
 open(a.output,'w').write(json.dumps(result,indent=2)+'\n');print(json.dumps({k:v for k,v in result.items() if k!='requests'}))
 if ok!=a.count:raise SystemExit(1)
if __name__ == '__main__':
 a=p.parse_args()
 tls_context=ssl.create_default_context(cafile=a.tls_ca) if a.tls_ca else None
 services=[int(s) for s in a.services.split(',')];gate=asyncio.Semaphore(a.concurrency);base=int(ipaddress.IPv4Address('198.18.81.1'));results=[]
 asyncio.run(main())
