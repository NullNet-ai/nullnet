import re,json,pathlib,statistics
p=pathlib.Path('/private/tmp/nullnet-vxlan-capacity');allrows=[]
for h in ['103','104']:
 for label in ['unique','shared']:
  stacks={};vals={}
  for line in (p/f'kernel-{h}-{label}.trace').read_text().splitlines():
   m=re.match(r'\s*(\d+)\)\s*(.*?)\|(.*)',line)
   if not m:continue
   cpu,dur,body=m.groups();depth=len(body)-len(body.lstrip());body=body.strip()
   if body.endswith('{'):
    stacks[depth]=body.split('(')[0].split(' [')[0].strip();continue
   if body.startswith('}'):
    c=re.search(r'/\* ([a-zA-Z_0-9]+)',body)
    fn=c.group(1) if c else stacks[depth]
   elif body.endswith(';'):fn=body.split('(')[0].split(' [')[0].strip()
   else:continue
   d=re.search(r'([\d.]+) us',dur)
   if d:vals.setdefault(fn,[]).append(float(d.group(1)))
  v={'host':h,'ports':label,'functions':{k:{'calls':len(a),'total_seconds':sum(a)/1e6,'mean_ms':statistics.mean(a)/1000} for k,a in vals.items()}}
  allrows.append(v);print(json.dumps(v))
(p/'kernel-summary.json').write_text(json.dumps(allrows,indent=2))
