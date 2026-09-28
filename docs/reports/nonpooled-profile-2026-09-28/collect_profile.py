import subprocess,json,sys,ast,pathlib,statistics,re
label,start=sys.argv[1:]
raw=subprocess.check_output(['journalctl','-u','nullnet-client','--since','@'+start,'-o','cat','--no-pager'],text=True)
lines=[x for x in raw.splitlines() if x.startswith('[nnprofile]')]
pathlib.Path('/tmp/nn28-phases-'+label+'.txt').write_text('\n'.join(lines)+'\n')
groups={};batches={};malformed=[]
for l in lines:
 _,op,*rest=l.split(' ',3)
 if op=='cleanup_wait':
  values=ast.literal_eval(l.split(' ',2)[2]);groups.setdefault('cleanup_queue',[]).extend(values)
 elif op in ['cleanup_delete','cleanup_bridge']:
  count,duration=map(int,l.split()[2:]);batches.setdefault(op,[]).append({'count':count,'us':duration})
 else:
  try: stages=ast.literal_eval(rest[1])
  except (ValueError,SyntaxError,IndexError):
   malformed.append(l);continue
  for stage,us in stages:groups.setdefault(op+'.'+stage,[]).append(us)
def summary(v):
 v=sorted(v);return {'count':len(v),'sum_s':sum(v)/1e6,'mean_ms':statistics.mean(v)/1000,'p50_ms':v[len(v)//2]/1000,'p95_ms':v[int(len(v)*.95)]/1000,'max_ms':v[-1]/1000}
r={'malformed_lines':len(malformed),'stages':{k:summary(v) for k,v in groups.items()},'batches':{k:{'summary':summary([x['us'] for x in v]),'endpoints':sum(x['count'] for x in v),'batches':v} for k,v in batches.items()}}
pathlib.Path('/tmp/nn28-phases-'+label+'.json').write_text(json.dumps(r,indent=2));print(json.dumps(r))
