import pathlib,re,json,collections,statistics
P=pathlib.Path('/private/tmp/nullnet-attribution')
rx=re.compile(r'^([\d.]+)\s*\|\s*(\d+)\)\s*(.*?)\s*\|\s*(.*?)\|\s*(.*)')
def union(a):
 total=0;end=-1e99
 for s,e in sorted(a):
  if e>end:total+=e-max(end,s);end=e
 return total
for h in ['103','104']:
 rows=[]
 for line in (P/f'kernel-{h}.trace').read_text().splitlines():
  m=rx.match(line)
  if not m:continue
  ts,cpu,task,d,body=m.groups();dur=re.search(r'([\d.]+) us',d)
  if not dur:continue
  dur=float(dur.group(1))/1e6;ts=float(ts)
  if body.startswith('}'):
   fn=re.search(r'/\* (\w+)',body).group(1);start=ts-dur;end=ts
  elif body.endswith(';'):
   fn=body.split('(')[0].split(' [')[0];start=ts;end=ts+dur
  else:continue
  rows.append({'fn':fn,'start':start,'end':end,'seconds':dur,'task':task,'pid':int(task.rsplit('-',1)[1])})
 by=collections.defaultdict(list)
 for r in rows:by[r['fn']].append(r)
 summary={k:{'calls':len(a),'sum_seconds':sum(r['seconds'] for r in a),'union_seconds':union([(r['start'],r['end']) for r in a]),'mean_ms':statistics.mean(r['seconds'] for r in a)*1000,'max_ms':max(r['seconds'] for r in a)*1000} for k,a in by.items()}
 (P/f'kernel-{h}-parsed.json').write_text(json.dumps(rows))
 (P/f'kernel-{h}-summary.json').write_text(json.dumps(summary,indent=2))
 print(h,json.dumps(summary,indent=2))
