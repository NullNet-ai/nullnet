import subprocess,json,sys,re
raw=subprocess.check_output(['journalctl','-u','nullnet-client','--since','@'+sys.argv[1],'-o','json','--no-pager'],text=True)
a=[];b=[];invalid=0
for line in raw.splitlines():
 e=json.loads(line);m=e.get('MESSAGE','')
 if m.startswith('CAPACITY_PROFILE '):
  try:d=json.loads(m.split(' ',1)[1])
  except json.JSONDecodeError:
   invalid+=1;continue
  d['epoch']=int(e['__REALTIME_TIMESTAMP'])/1e6;a.append(d)
 if m.startswith('CAPACITY_CONTROL '):
  d={k:float(v) for k,v in re.findall(r'(\w+)=([\d.]+)',m)};b.append(d)
print(json.dumps({'endpoint':a,'control':b,'invalid_records':invalid}))
