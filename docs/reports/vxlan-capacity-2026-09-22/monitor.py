import time,json,pathlib
p=pathlib.Path('/tmp/nullnet-capacity-monitor.jsonl')
def cpu():return list(map(int,pathlib.Path('/proc/stat').read_text().splitlines()[0].split()[1:]))
last=cpu()
with p.open('w') as f:
 for i in range(130):
  time.sleep(1);now=cpu();d=[b-a for a,b in zip(last,now)];total=sum(d[:8]);row={'epoch':time.time(),'cpu_busy_percent':100*(total-d[3]-d[4])/total,'iowait_percent':100*d[4]/total,'vxlan_devices':sum(p.name.startswith('vxlan-') for p in pathlib.Path('/sys/class/net').iterdir()),'mem_available_kib':int(next(l for l in pathlib.Path('/proc/meminfo').read_text().splitlines() if l.startswith('MemAvailable:')).split()[1])};f.write(json.dumps(row)+'\n');f.flush();last=now
