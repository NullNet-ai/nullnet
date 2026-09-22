import asyncio,json,time,socket,ipaddress,resource,statistics
resource.setrlimit(resource.RLIMIT_NOFILE,(65536,resource.getrlimit(resource.RLIMIT_NOFILE)[1]))
services=json.load(open('/tmp/nullnet-review-services.json'));results=[]
async def main():
 start=time.monotonic()
 async def one(i):
  await asyncio.sleep(max(0,start+i/1000-time.monotonic()));beg=time.monotonic();w=None;status=0;error=None
  try:
   r,w=await asyncio.wait_for(asyncio.open_connection('192.168.1.104',80,local_addr=(str(ipaddress.IPv4Address('198.19.0.1')+i),0)),90)
   w.write(('GET / HTTP/1.1\r\nHost: '+services[i%len(services)]['name']+'\r\nConnection: close\r\n\r\n').encode());await w.drain();body=await asyncio.wait_for(r.read(),90);status=int(body.split(b' ',2)[1])
  except Exception as e:error=repr(e)
  finally:
   if w:w.close()
  results.append({'i':i,'start':beg-start,'end':time.monotonic()-start,'ms':(time.monotonic()-beg)*1000,'status':status,'error':error})
 await asyncio.gather(*(one(i) for i in range(1000)))
 elapsed=time.monotonic()-start;ok=[r for r in results if r['status']==200];ts=sorted(r['ms'] for r in ok)
 output={'offered_per_second':1000,'requests':1000,'success':len(ok),'errors':1000-len(ok),'seconds':elapsed,'ready_per_second':len(ok)/elapsed,'mean_ms':statistics.mean(ts) if ts else None,'p99_ms':ts[int(len(ts)*.99)] if ts else None,'results':results}
 open('/tmp/nullnet-capacity-full.json','w').write(json.dumps(output));print(json.dumps({k:v for k,v in output.items() if k!='results'}),flush=True)
asyncio.run(main())
