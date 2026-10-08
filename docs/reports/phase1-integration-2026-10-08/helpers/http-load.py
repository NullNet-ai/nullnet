"""C256 LAN-only cold/warm HTTP workload with explicit client identities."""
import argparse,asyncio,ipaddress,json,time,resource,re
soft,hard=resource.getrlimit(resource.RLIMIT_NOFILE)
resource.setrlimit(resource.RLIMIT_NOFILE,(min(hard,524288),hard))
p=argparse.ArgumentParser();p.add_argument('--count',type=int,default=1008);p.add_argument('--concurrency',type=int,default=256);p.add_argument('--identities',type=int,default=1008);p.add_argument('--offset',type=int,default=0);p.add_argument('--services',default='1,2,3,4,5,6');p.add_argument('--path',default='/');p.add_argument('--output',required=True);a=p.parse_args()
services=[int(s) for s in a.services.split(',')];gate=asyncio.Semaphore(a.concurrency);base=int(ipaddress.IPv4Address('198.18.81.1'));results=[]
async def request(n):
 async with gate:
  started=time.monotonic();writer=None;source=str(ipaddress.IPv4Address(base+a.offset+n%a.identities));service=services[n%len(services)]
  try:
   reader,writer=await asyncio.wait_for(asyncio.open_connection('192.168.1.104',80,local_addr=(source,0)),90)
   connected=time.monotonic()
   path='/tree' if a.path=='/tree-all' else a.path
   writer.write(f'GET {path} HTTP/1.1\r\nHost: p1-{service:02d}.test\r\nConnection: close\r\n\r\n'.encode());await writer.drain()
   header=await asyncio.wait_for(reader.readuntil(b'\r\n\r\n'),120);headers_received=time.monotonic();status=int(header.split(b' ')[1]);length=None
   for line in header.split(b'\r\n'):
    if line.lower().startswith(b'content-length:'):length=int(line.split(b':',1)[1])
   body=await asyncio.wait_for(reader.readexactly(length) if length is not None else reader.read(),120)
   good=status==200 and (a.path!='/tree' or b'node=' in body)
   if a.path=='/tree-all':good=good and sorted(re.findall(rb'node=(\d+)',body))==[f'{i:02d}'.encode() for i in range(1,13)]
   if a.path=='/payload.bin':good=good and body==bytes(range(256))*4096
   results.append({'n':n,'source':source,'service':service,'status':status,'ok':good,'bytes':len(body),'seconds':time.monotonic()-started,'connect_ms':(connected-started)*1000,'response_headers_ms':(headers_received-connected)*1000,'response_body_ms':(time.monotonic()-headers_received)*1000})
  except Exception as e:results.append({'n':n,'source':source,'service':service,'ok':False,'error':type(e).__name__+': '+str(e),'seconds':time.monotonic()-started})
  finally:
   if writer:
    writer.close()
    try:await writer.wait_closed()
    except (ConnectionError,OSError):pass
async def main():
 start=time.monotonic();unix_start=time.time();await asyncio.gather(*(request(n) for n in range(a.count)));elapsed=time.monotonic()-start;rows=sorted(results,key=lambda r:r['seconds']);ok=sum(r['ok'] for r in rows)
 result={'unix_start':unix_start,'unix_end':time.time(),'count':a.count,'concurrency':a.concurrency,'identities':a.identities,'services':services,'ok':ok,'errors':a.count-ok,'request_seconds':elapsed,'requests_per_second':a.count/elapsed,'p50_ms':rows[len(rows)//2]['seconds']*1000,'p95_ms':rows[int(len(rows)*.95)]['seconds']*1000,'requests':results}
 open(a.output,'w').write(json.dumps(result,indent=2)+'\n');print(json.dumps({k:v for k,v in result.items() if k!='requests'}))
 if ok!=a.count:raise SystemExit(1)
asyncio.run(main())
