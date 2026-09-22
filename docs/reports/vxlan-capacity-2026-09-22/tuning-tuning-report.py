import json,pathlib,statistics,collections,math
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
from matplotlib.backends.backend_pdf import PdfPages
base=pathlib.Path('/private/tmp/nullnet-attribution')
out=pathlib.Path('/Users/giulianobellini/Desktop/GitHub/nullnet/docs/reports/vxlan-capacity-2026-09-22')
allrows=[]
for host in ['103','104']:
 for suite in ['sweep','refine','variants','equivalence']:
  p=base/(suite+'-'+host+'.json')
  if p.exists():
   for r in json.loads(p.read_text()):allrows.append(dict(r,host=host,suite=suite))
import gzip
if not allrows and (out/'bare-tuning-raw.json.gz').exists():
 with gzip.open(out/'bare-tuning-raw.json.gz','rt') as f:allrows=json.load(f)
with gzip.open(out/'bare-tuning-raw.json.gz','wt') as f:json.dump(allrows,f)
(out/'bare-tuning-measurements.json').write_text(json.dumps([{k:v for k,v in r.items() if k!='endpoints'} for r in allrows],indent=2))
conf=[r for r in allrows if r['phase']=='confirm']
groups=collections.defaultdict(list)
for r in conf:groups[(r['workers'],min(r['workers'],r['slots']),r['host'])].append(r)
pairs=sorted({k[:2] for k in groups})
summary=[]
for w,s in pairs:
 item={'workers':w,'slots':s}
 for h in ['103','104']:
  rr=groups[(w,s,h)]
  if rr:item[h]={'rate':statistics.median(r['rate'] for r in rr),'min':min(r['rate'] for r in rr),'max':max(r['rate'] for r in rr),'p99_ms':statistics.median(r['seconds_p99']*1000 for r in rr),'burst_completion_p99_seconds':statistics.median(r['completed_seconds_p99'] for r in rr),'n':len(rr)}
 if '103' in item and '104' in item:item['mean_rate']=(item['103']['rate']+item['104']['rate'])/2;summary.append(item)
summary.sort(key=lambda x:x['mean_rate'],reverse=True)
(out/'bare-tuning-summary.json').write_text(json.dumps(summary,indent=2))
with PdfPages(out/'nullnet-bare-tuning.pdf') as pdf:
 fig,axes=plt.subplots(1,2,figsize=(11.7,8.3))
 for ax,h in zip(axes,['103','104']):
  rows=[r for r in allrows if r['phase']=='coarse' and r['host']==h];ws=sorted({r['workers'] for r in rows});ss=sorted({r['slots'] for r in rows})
  matrix=[[next((r['rate'] for r in rows if r['workers']==w and r['slots']==s),float('nan')) for s in ss] for w in ws]
  im=ax.imshow(matrix,cmap='YlGnBu',vmin=100,vmax=450,aspect='auto')
  for i,row in enumerate(matrix):
   for j,v in enumerate(row):
    if not math.isnan(v):ax.text(j,i,f'{v:.0f}',ha='center',va='center',color='white' if v>330 else 'black')
  ax.set_xticks(range(len(ss)),ss);ax.set_yticks(range(len(ws)),ws);ax.set_xlabel('Subprocess slots');ax.set_ylabel('Setup workers');ax.set_title('Lab '+h+' — endpoints/s')
 fig.suptitle('Bare encrypted Docker endpoint setup: concurrency sweep',fontsize=17)
 fig.text(.07,.89,'Workers = endpoint setups running concurrently. Subprocess slots = external commands running concurrently.\nNative Netlink calls do not use subprocess slots. Example: 32/8 allows 32 setups, with at most 8 commands at once.',fontsize=10)
 fig.text(.07,.065,'256 endpoints per cell; cached Docker PID; iptables outside timer; repeated sysctl retained.\nEach endpoint has its own VXLAN, bridge, veth pair, UDP port and four XFRM objects.\nBlank cells were not tested. Short runs screen candidates; use 1,000-endpoint repeats for selection.',fontsize=10)
 fig.subplots_adjust(top=.83,bottom=.19,wspace=.28);fig.savefig(out/('tuning-page-'+str(pdf.get_pagecount()+1)+'.png'),dpi=130);pdf.savefig(fig);plt.close(fig)
 fig,ax=plt.subplots(figsize=(11.7,8.3))
 labels=[str(x['workers'])+'/'+str(x['slots']) for x in summary];yy=list(range(len(summary)))
 for offset,h,color in [(-.18,'103','#187c9b'),(.18,'104','#f09345')]:
  vals=[x[h]['rate'] for x in summary]
  ax.barh([y+offset for y in yy],vals,.34,label='Lab '+h,color=color,xerr=[[x[h]['rate']-x[h]['min'] for x in summary],[x[h]['max']-x[h]['rate'] for x in summary]],capsize=3)
 ax.set_yticks(yy,labels);ax.invert_yaxis();ax.set_xlabel('Median endpoints/s, 1,000 endpoints per run');ax.set_ylabel('Workers / effective command slots');ax.legend()
 ax.set_title('PID/iptables-only changes: 8/8 is a practical candidate',fontsize=17)
 fig.text(.07,.065,'Bars: median; whiskers: observed min/max, not confidence intervals. Slots above workers are grouped.\nAll runs check object counts, Docker PID/start-time continuity, and removal of test interfaces.\nRates are endpoint creation throughput, not complete cross-host connections or HTTP requests.',fontsize=10)
 fig.subplots_adjust(bottom=.19,left=.15);fig.savefig(out/('tuning-page-'+str(pdf.get_pagecount()+1)+'.png'),dpi=130);pdf.savefig(fig);plt.close(fig)
 phases=['baseline','sysctl-once','inline-hash','namespace-batch','xfrm-batch','optimized-confirm']
 names=['No PID / iptables per endpoint','+ sysctl once','+ in-process SHA-256','+ namespace ip batch','+ XFRM ip batch','+ 8 workers / 8 slots']
 fig,ax=plt.subplots(figsize=(11.7,8.3))
 variant_summary={}
 for offset,h,color in [(-.18,'103','#187c9b'),(.18,'104','#f09345')]:
  vals=[]
  for phase in phases:
   rr=[r for r in allrows if r['n']==1000 and r['suite'] in ['variants','equivalence'] and r['host']==h and r['phase']==phase]
   value=statistics.median(r['rate'] for r in rr) if rr else 0;vals.append(value)
   variant_summary[h+' '+phase]=value
  bars=ax.barh([i+offset for i in range(len(phases))],vals,.34,label='Lab '+h,color=color)
  ax.bar_label(bars,fmt='%.0f',padding=4)
 ax.set_yticks(range(len(phases)),names);ax.invert_yaxis();ax.set_xlabel('Median endpoints/s; cumulative changes, 32/8 except last row (8/8)');ax.legend();ax.set_xlim(right=max(variant_summary.values())*1.18)
 ax.set_title('Required operations, fewer subprocesses',fontsize=17)
 fig.text(.07,.065,'Two 1,000-endpoint runs per case and host. Namespace batch preserves address-before-UP;\nXFRM batch preserves the four state/policy operations and order. No Nullnet code changes.\nBare setup checks do not replace encrypted traffic, restart, partial-failure and lifecycle testing.',fontsize=10)
 fig.subplots_adjust(bottom=.19,left=.29);fig.savefig(out/('tuning-page-'+str(pdf.get_pagecount()+1)+'.png'),dpi=130);pdf.savefig(fig);plt.close(fig)
 fig,ax=plt.subplots(figsize=(11.7,8.3))
 for offset,h,color in [(-.18,'103','#187c9b'),(.18,'104','#f09345')]:
  means=[];lo=[];hi=[]
  for phase in ['optimized-confirm','optimized-control']:
   rr=[r for r in allrows if r['suite']=='equivalence' and r['host']==h and r['phase']==phase]
   vv=[r['rate'] for r in rr];v=statistics.median(vv) if vv else 0
   means.append(v);lo.append(v-min(vv) if vv else 0);hi.append(max(vv)-v if vv else 0)
  bars=ax.barh([i+offset for i in range(2)],means,.34,label='Lab '+h,color=color,xerr=[lo,hi],capsize=3)
  ax.bar_label(bars,fmt='%.0f',padding=8)
 ax.set_xlim(right=710)
 ax.set_yticks(range(2),['8 workers / 8 slots','32 workers / 8 slots']);ax.invert_yaxis();ax.set_xlabel('Median endpoints/s, complete batched setup');ax.legend()
 ax.set_title('Batched setup: retain 32 workers / 8 subprocess slots',fontsize=16)
 fig.text(.07,.065,'Two 1,000-endpoint trials per pair and host, ordered 8 / 32 / 32 / 8 workers. Both use 8 command slots.\nWhiskers show observed min/max. The lab is KVM-based and later sweep trials slowed on both nodes;\n32/8 was more consistent here; 8/8 matched it once. Neither is proven a universal optimum.',fontsize=10)
 fig.subplots_adjust(bottom=.19,left=.23);fig.savefig(out/('tuning-page-'+str(pdf.get_pagecount()+1)+'.png'),dpi=130);pdf.savefig(fig);plt.close(fig)
print(json.dumps({'finalists':summary,'variants':variant_summary},indent=2))
