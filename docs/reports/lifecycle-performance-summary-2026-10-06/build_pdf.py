from pathlib import Path
import json, textwrap, hashlib
import runpy
import numpy as np
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
from matplotlib.backends.backend_pdf import PdfPages
from matplotlib.colors import Normalize

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
SOURCES = {
    'cpu': ROOT/'lifecycle-cpu-2026-10-05/summary.json',
    'events': ROOT/'device-event-bypass-2026-10-05/summary.json',
    'fresh': ROOT/'device-event-bypass-2026-10-05/fine-lock-2026-10-06-summary.json',
    'pool': ROOT/'retained-cpu-lock-2026-10-06/summary.json',
    'bridgefree': ROOT/'bridgefree-lifecycle-2026-10-08/summary.json',
}
D = {k: json.loads(p.read_text()) for k,p in SOURCES.items()}
BC = runpy.run_path(str(ROOT/'bridgefree-lifecycle-2026-10-08/columns.py'))
BF = D['bridgefree']
def bf(host,lifecycle,kind='cpu'):
    return BC['dataset'](BF,host,lifecycle,kind)
def bp(host,lifecycle):
    return BF['hosts'][host]['conditions'][lifecycle+'-redirect']['concurrency']['256']['performance']
BLUE, TEAL, ORANGE, INK, MUTED = '#2563a6','#00877e','#c65f22','#172c40','#526779'
plt.rcParams.update({'font.family':'DejaVu Sans','font.size':10,'axes.spines.top':False,'axes.spines.right':False,'axes.labelcolor':INK,'text.color':INK,'xtick.color':MUTED,'ytick.color':MUTED,'pdf.fonttype':42})
VM_LABELS = {'103': 'VM1', '104': 'VM2'}
PDF = HERE/'nullnet-cpu-rtnl-breakdown.pdf'
page_no = 0
pdf = PdfPages(PDF, metadata={'Title':'Nullnet lifecycle performance: CPU and RTNL breakdown','Author':'Nullnet performance analysis','Subject':'Fresh, pooled and bridge-free device measurements, October 5–8, 2026'})

def page(title, subtitle, source):
    global page_no
    page_no += 1
    fig=plt.figure(figsize=(8.27,11.69),facecolor='white')
    fig.text(.075,.952,'NULLNET  /  PERFORMANCE ANALYSIS',fontsize=9,color=TEAL,weight='bold')
    fig.text(.075,.908,title,fontsize=22,weight='bold')
    para(fig,.875,subtitle,size=10.5)
    fig.text(.075,.038,source,fontsize=7,color=MUTED)
    fig.text(.925,.038,str(page_no),ha='right',fontsize=9,color=MUTED)
    return fig

def para(fig,y,text,size=11,width=94,color=INK):
    lines=[]
    for p in text.split('\n'):
        lines.extend(textwrap.wrap(p,width=width) or [''])
    fig.text(.075,y,'\n'.join(lines),fontsize=size,va='top',linespacing=1.45,color=color)
    return y-len(lines)*size*1.45/(11.69*72)

def label(fig,y,title,text):
    fig.text(.075,y,title,fontsize=13,weight='bold',va='top',color=TEAL)
    return para(fig,y-.031,text)

def save(fig):
    pdf.savefig(fig)
    plt.close(fig)

def ax(fig,box): return fig.add_axes(box)

def bars(a,labels,vals,color=BLUE,xlabel='ms per completed local cycle', digits=3):
    a.barh(np.arange(len(vals)),vals,color=color,height=.65)
    a.set_yticks(np.arange(len(vals)),labels,fontsize=9)
    a.invert_yaxis();a.set_xlabel(xlabel);a.grid(axis='x',alpha=.15);a.set_axisbelow(True)
    a.set_xlim(0,max(vals)*1.23)
    for i,v in enumerate(vals): a.text(v+max(vals)*.015,i,f'{v:.{digits}f}',va='center',fontsize=9)

def heat(a,labels,cols,values,vmax=None,fmt='.3f'):
    values=np.array(values)
    im=a.imshow(values,aspect='auto',cmap='YlOrRd',vmin=0,vmax=vmax or values.max())
    a.set_yticks(range(len(labels)),labels,fontsize=8.5)
    a.set_xticks(range(len(cols)),cols,fontsize=9);a.xaxis.tick_top()
    for i in range(len(labels)):
        for j in range(len(cols)):
            v=values[i,j];a.text(j,i,format(v,fmt),ha='center',va='center',fontsize=8.5,weight='bold' if labels[i]=='Total' else 'normal',color='white' if v>im.norm.vmax*.62 else INK)
    if labels[-1]=='Total':
        a.axhline(len(labels)-1.5,color=INK,lw=1.2)
        a.get_yticklabels()[-1].set_weight('bold')
    for spine in a.spines.values(): spine.set_visible(False)
    a.tick_params(length=0)
    return im

def table(fig,box,headers,rows,widths=None,size=9,heatmap=False):
    a=ax(fig,box);a.axis('off')
    t=a.table(cellText=rows,colLabels=headers,colWidths=widths,cellLoc='right',loc='upper left',bbox=[0,0,1,1])
    t.auto_set_font_size(False);t.set_fontsize(size)
    if heatmap:
        norm=Normalize(0,max(float(v) for row in rows if 'subtotal' not in row[0].lower() and not row[0].startswith('Total') for v in row[1:]))
    for (r,c),cell in t.get_celld().items():
        cell.set_linewidth(.3);cell.set_edgecolor('#dbe3e9')
        if r==0:cell.set_facecolor(INK);cell.set_text_props(color='white',weight='bold')
        else:cell.set_facecolor('#f0f5f7' if r%2 else 'white')
        if heatmap and r>0:
            summary='subtotal' in rows[r-1][0].lower() or rows[r-1][0].startswith('Total')
            if summary:
                cell.set_facecolor('#dbe8ef');cell.set_text_props(weight='bold')
            elif c>0:
                value=float(rows[r-1][c])
                cell.set_facecolor(plt.get_cmap('YlOrRd')(norm(value)))
                cell.set_text_props(color='white' if value>norm.vmax*.62 else INK)
        if r>0 and str(rows[r-1][0]).startswith('Total'):
            cell.set_facecolor('#dbe8ef');cell.set_text_props(weight='bold')
        if c==0:cell.set_text_props(ha='left')
    if heatmap:
        fig.text(box[0],box[1]+box[3]+.012,'Heatmap: darker = larger ms/cycle; shared VM scale per page; subtotals/totals excluded.',fontsize=7.5,color=MUTED)
    return t

OPERATION_REASONS = {
 'Device creation': 'Build this dedicated endpoint; retained devices skip it.',
 'TC installation': 'Attach the packet steering/isolation programs; pools keep them.',
 'IPsec install/remove': 'Encrypt the edge; remove old keys/policy on retirement.',
 'Addresses': 'Give the endpoint IP connectivity; clear addresses on reset.',
 'Attach / enable': 'Connect the forwarding path and make interfaces operational.',
 'Disable links': 'Quiesce the old endpoint during teardown/reset.',
 'Detach ports': 'Disconnect retained bridge ports in the current reset design.',
 'GETLINK queries': 'Resolve device IDs and synchronize readiness; verify reset.',
 'Batch deletion': 'Reclaim fresh devices; retained devices skip deletion.',
 'Conntrack deletes': 'Clear old flow/NAT state before reuse; tables are empty here.',
 'Neighbor dump': 'Find stale ARP/NDP entries to clear; tables are empty here.',
 'Peer routes': 'Make remote endpoint/gateway IPs reachable from the host.',
}

def operation_table(fig,box,descriptions):
    rows=[[name,textwrap.fill(desc,35),textwrap.fill(OPERATION_REASONS[name],43)] for name,desc in descriptions]
    t=table(fig,box,['Operation family','What it does','Why needed'],rows,[.25,.34,.41],8)
    for (r,c),cell in t.get_celld().items():
        cell.set_text_props(ha='left')
    return t

def fv(host,key): return D['fresh'][f'host-{host}']['rows'][key]['hold_ms_per_cycle']
def pv(host,key,kind='locks'): return D['pool'][f'host-{host}'][kind]['256']['rows'][key]['hold_ms_per_cycle' if kind=='locks' else 'cpu_ms_per_cycle']
def cpu(host):return D['cpu']['hosts'][host]['concurrent_request_cpu_ms']
def filtered(host):return next(r for r in D['events'][host]['final'] if r['label']=='events-final-c256')
def host_cpu(r):return sum(r['cpu_delta'][i] for i in [0,1,2,5,6])*10/r['cycles']

# Cover
page_no += 1
f=plt.figure(figsize=(8.27,11.69),facecolor='white')
f.text(.09,.92,'NULLNET  /  PERFORMANCE ANALYSIS',fontsize=10,color=TEAL,weight='bold')
a=ax(f,[.09,.765,.12,.007]);a.set_facecolor(TEAL);a.set_xticks([]);a.set_yticks([])
for spine in a.spines.values():spine.set_visible(False)
f.text(.09,.69,'Lifecycle\nperformance',fontsize=38,weight='bold',va='top',linespacing=1.2)
f.text(.09,.535,'CPU work and RTNL serialization',fontsize=19,color=TEAL)
f.text(.09,.455,'Fresh, pooled and bridge-free veth / VXLAN endpoints\nOperation costs, resource limits and paths to parallelism',fontsize=12,color=MUTED,linespacing=1.7)
f.text(.09,.35,'KERNEL MEASUREMENTS',fontsize=9,color=TEAL,weight='bold')
f.text(.09,.305,'October 5–8, 2026\nVM1 and VM2 · 8 vCPUs each · Linux 6.12.95\nHistorical fixture scope · Concurrency 256',fontsize=11,color=INK,linespacing=1.8,va='top')
f.text(.09,.185,'READING GUIDE',fontsize=9,color=TEAL,weight='bold')
f.text(.09,.15,'Summary p2 · Definitions p3 · CPU charts p4–6\nRTNL charts p7–8 · Resource limits p9\nComplete operation tables p10–13 · Next steps p14\nBridge-free columns use the same concurrency: 256',fontsize=10,color=MUTED,linespacing=1.7,va='top')
f.text(.09,.065,'Dedicated encrypted endpoint lifecycle',fontsize=9,color=MUTED)
f.text(.925,.038,str(page_no),ha='right',fontsize=9,color=MUTED)
save(f)

# Summary
f=page('Where performance stands','Fresh and pooled devices • C256 throughout • Bridge-free columns added October 8, 2026','Sources: Oct 5–6 CPU/RTNL reports; Oct 8 bridgefree-lifecycle comparison.')
para(f,.81,'Bridge-free forwarding substantially reduces pooled RTNL work while keeping dedicated per-edge encryption. The matched C256 comparison improves fresh-device turnover by 26–27% and pooled turnover by 79–81%.',size=13,width=76)
rows=[]
for name,h in [('Fresh · VM1','103'),('Fresh · VM2','104'),('Pooled · VM1','103'),('Pooled · VM2','104')]:
    if name.startswith('Fresh'):
        r=filtered(h); rate=r['cycles_per_second'];hc=host_cpu(r);lock=D['fresh'][f'host-{h}']['fixture_hold_ms_per_cycle']
    else:
        r=D['pool'][f'host-{h}']['performance']['256']['all'];rate=r['cycles_per_second'];hc=r['host_cpu_ms_per_cycle'];lock=D['pool'][f'host-{h}']['locks']['256']['fixture_hold_ms_per_cycle']
    lifecycle='unpooled' if name.startswith('Fresh') else 'pooled'
    r=bp(h,lifecycle)
    rows.append([name,f'{rate:.0f}',f'{hc:.2f}',f'{lock:.2f}',f'{r["cycles_per_second"]:.0f}',f'{r["host_cpu_ms_per_cycle"]:.2f}',f'{bf(h,lifecycle,"locks")["fixture_hold_ms_per_cycle"]:.2f}'])
table(f,[.075,.55,.85,.17],['C256','B cycles/s','B CPU ms','B RTNL ms','NB cycles/s','NB CPU ms','NB RTNL ms'],rows,[.25,.125,.125,.125,.125,.125,.125],8)
para(f,.525,'B = historical bridge fixture (Oct 5–6); NB = bridge-free (Oct 8). Gains above use the matched Oct 8 bridged reruns, not ratios between these dates. CPU and RTNL overlap and must not be added. Rates count local activation/reset cycles, not requests or two-ended edges.',size=9)
label(f,.425,'1  CPU overhead is real','Udev owned about 44% of sampled unfiltered host CPU. Early scoped filtering removes the event-worker storm and substantially improves fresh-device throughput.')
label(f,.315,'2  Bridge-free pools remove the main lock hotspot','C256 pooled bridge-free RTNL is 0.530 / 0.503 ms/cycle, about 67% below the matched bridged reruns. Fixed redirects stay installed across leases; fresh keys and link-state changes remain.')
label(f,.205,'3  Preserve the comparison scope','The same historical exact-tuple conntrack deletes and known-address reset are used. Extra gateway rules/routes are included. Product-wide table dumps, controller queues and persistence remain integration work; these are kernel rates.')
save(f)
# 2
f=page('Three clocks, two resource limits','An endpoint’s lifecycle includes CPU work and RTNL waiting; CPU execution can overlap RTNL hold time.','Sources: measurement definitions in all four October reports.')
label(f,.80,'CPU time: active execution','Sum of time executing on CPUs. Host CPU includes all processes and kernel activity across eight vCPUs; process CPU includes fixture threads and their kernel execution. Sleeping time is excluded.')
label(f,.66,'RTNL hold time: a serialized resource','Elapsed time between acquiring and releasing the global networking lock. Sleeps, preemption and RCU waits while holding it still block other users. It overlaps CPU execution; it is not an extra CPU charge.')
label(f,.52,'Request latency and lock wait: elapsed delay','A request may wait behind many concurrent callers before acquiring RTNL. Summed waits overlap across threads: 1,025 ms/cycle of aggregate acquisition wait in one trace does not mean one second of serial work per cycle.')
a=ax(f,[.20,.265,.70,.14]);a.set_xlim(0,10);a.set_ylim(-.85,3.05);a.axis('off')
for y,name,segments in [
    (2,'Request',[(0,10,MUTED)]),
    (1,'CPU active',[(0,1,BLUE),(4,2,BLUE),(8,2,BLUE)]),
    (0,'RTNL held',[(4,5,ORANGE)]),
]:
    a.text(-.25,y,name,ha='right',va='center',fontsize=9)
    for begin,length,col in segments:
        a.broken_barh([(begin,length)],(y-.18,.36),facecolors=col)
for x in [1,4,6,8,9]:
    a.plot([x,x],[-.23,2.25],color='#dbe3e9',lw=.7,zorder=0)
a.text(2.5,2.55,'Wait for RTNL',ha='center',fontsize=8)
a.text(6.5,2.55,'Own RTNL',ha='center',fontsize=8,color=ORANGE)
a.text(2.5,1,'asleep',ha='center',va='center',fontsize=8,color=MUTED)
a.text(7,1,'RCU wait*',ha='center',va='center',fontsize=8,color=MUTED)
a.text(4,-.48,'Acquire',ha='center',fontsize=8)
a.text(9,-.48,'Release',ha='center',fontsize=8)
f.text(.075,.246,'*Illustrative sleep: caller uses no CPU; RTNL stays held. Timeline is not measured.',fontsize=8,color=MUTED)
para(f,.21,'One cycle = one local endpoint setup/activation plus completed teardown/reset. A cross-host edge needs an endpoint on each host; per-host rates are not doubled to claim cross-host edge throughput. C is wave width; mixed trials can overlap C activations and C retirements.',size=10)
para(f,.105,'Timed loops exclude initial pool preparation, final destruction, control RPCs, history/storage and application traffic. Empty fixture flow/neighbor tables make these lifecycle diagnostics with empty flow tables.',size=9.5)
save(f)
# 3
f=page('Filtering helps fresh devices most','Matched baseline → udev-only → all audited consumers. Two balanced 15-second trials per cell.','Source: device-event-bypass-2026-10-05/summary.json, balanced trials.')
labels=[]; rates=[]; costs=[]
for c in [256]:
    for h in ['103','104']:
        labels.append(f'C{c} · {VM_LABELS[h]}')
        rr=[next(r for r in D['events'][h]['balanced'] if r['concurrency']==c and r['filter']==m) for m in ['baseline','udev','all']]
        rates.append([r['cycles_per_second'] for r in rr]);costs.append([r['host_cpu_ms_per_cycle'] for r in rr])
heat(ax(f,[.23,.60,.65,.17]),labels,['Baseline','Udev only','All listeners'],rates,fmt='.0f')
f.text(.075,.805,'THROUGHPUT · complete local cycles/s',fontsize=11,weight='bold')
heat(ax(f,[.23,.32,.65,.17]),labels,['Baseline','Udev only','All listeners'],costs,fmt='.2f')
f.text(.075,.525,'HOST CPU · ms per completed cycle',fontsize=11,weight='bold')
para(f,.255,'At C256, all-listener filtering improves throughput 26–35%. Udev provides most of that gain; adding the rtnetlink consumers does not improve throughput over udev-only in these matched trials.',size=10.5)
para(f,.14,'Darker cells mean larger values within each heatmap, not necessarily better performance. Subsequent preparation-filtered runs reached 380–382/s at C256; those are separate runs, not matched speedup estimates. Baseline udev drain timed out after completed comparison trials on both hosts.',size=9.5)
save(f)
# 4
f=page('Where the host CPU goes','Execution-owner sampling exposes the notification cost that request timers cannot see.','Sources: lifecycle-cpu sampled_cpu; bypass CPU sample (VM1, C256); pooled performance.')
a=ax(f,[.16,.55,.73,.22]);owners=['Lifecycle caller','Udev + descendants','Kernel workers','Other userspace'];colors=[BLUE,ORANGE,TEAL,'#91a5b6']
vals=[[D['cpu']['sampled_cpu'][k]['ms_per_cycle'] for k in ['lifecycle caller','udev and descendants','kernel workers','other userspace']],[4.700,0,1.476,.528]]
for i,row in enumerate(vals):
    left=0
    for value,col in zip(row,colors):
        a.barh(i,value,left=left,color=col,height=.46)
        if value>.8:a.text(left+value/2,i,f'{value:.2f}',ha='center',va='center',color='white',fontsize=10)
        left+=value
    a.text(left+.15,i,f'{left:.2f}',va='center',weight='bold')
a.set_yticks([0,1],['Unfiltered','Filtered']);a.invert_yaxis();a.set_xlim(0,18);a.set_xlabel('Estimated active host CPU ms per cycle');a.grid(axis='x',alpha=.15)
for i,(name,col) in enumerate(zip(owners,colors)):f.text(.075+(i%2)*.44,.475-(i//2)*.027,'■ '+name,color=col,fontsize=10)
para(f,.395,'Unfiltered: 3,328 cycles, 16.13 sampled CPU ms/cycle versus 16.56 from /proc/stat. Filtered: 6,144 cycles, 6.70 sampled versus 6.78 from /proc/stat. No lost samples; no udev samples after filtering is not a universal zero-CPU guarantee.',size=10)
label(f,.26,'Why pooled filtering has little payoff','Pooled timed trials registered zero raw uevent drops: devices are not repeatedly registered. Link/address notifications still occur. Filtering changes throughput about 0–1% at C256.')
para(f,.13,'Udev processes device hotplug events, rules and helpers. Avahi tracks interfaces/addresses for multicast DNS. Filtering only owned Nullnet virtual-device notifications preserves foreign, Docker and physical-device events. A udev rule runs after queueing; the socket filter prevents queueing/wakeup earlier.',size=10)
save(f)
# 5 grouped CPU
fresh_groups=[('Device creation',['setup/create_veth','setup/create_vxlan']),('TC installation',['setup/tc_filter_add','setup/tc_qdisc_add']),('IPsec install/remove',[k for k in cpu('103') if 'IPsec' in k]),('Addresses',['setup/address_add','teardown/address_remove']),('Attach / enable',['setup/enable_attach','setup/enable']),('Disable links',['teardown/disable']),('Detach ports',[]),('GETLINK queries',['setup/getlink_readiness','teardown/getlink_readiness']),('Batch deletion',['teardown/delete_link_batch']),('Conntrack deletes',['teardown/conntrack_delete']),('Neighbor dump',['teardown/neighbor_dump_inclusive']),('Peer routes',[])]
def pg(key):
 if 'ipsec' in key:return 'IPsec install/remove'
 if 'address' in key:return 'Addresses'
 if 'detach' in key:return 'Detach ports'
 if 'disable' in key:return 'Disable links'
 if any(s in key for s in ['attach','enable']):return 'Attach / enable'
 if 'readiness' in key:return 'GETLINK queries'
 if 'conntrack' in key:return 'Conntrack deletes'
 if 'neighbor' in key:return 'Neighbor dump'
 raise ValueError(key)
f=page('CPU work by operation family','Calling-thread CPU, C256. Fresh and pooled device lifecycles, concurrency 256.','Sources: lifecycle-cpu concurrent_request_cpu_ms; retained cpu/256/rows. Calling-thread active CPU.')
values=[]
for name,keys in fresh_groups:
 row=[sum(cpu(h)[k] for k in keys) for h in ['103','104']]
 row += [sum(v['cpu_ms_per_cycle'] for k,v in D['pool'][f'host-{h}']['cpu']['256']['rows'].items() if pg(k)==name) for h in ['103','104']]
 row += [sum(v['cpu_ms_per_cycle'] for k,v in bf(h,lifecycle)['rows'].items() if BC['family'](k)==name) for lifecycle in ['unpooled','pooled'] for h in ['103','104']]
 values.append(row)
family_labels=[name for name,_ in fresh_groups]
heat(ax(f,[.38,.55,.52,.22]),family_labels+['Total'],['Fresh\nB VM1','Fresh\nB VM2','Pool\nB VM1','Pool\nB VM2','Fresh\nNB VM1','Fresh\nNB VM2','Pool\nNB VM1','Pool\nNB VM2'],values+[np.sum(values,axis=0).tolist()],vmax=np.max(values))
descriptions=[
 ['Device creation','Create the veth pair and VXLAN interface.'],
 ['TC installation','Add packet-hook attachment points and TC filters.'],
 ['IPsec install/remove','Add/remove encryption states and the outbound policy.'],
 ['Addresses','Add/remove endpoint IP addresses on interfaces.'],
 ['Attach / enable','Join interfaces to the bridge and set interfaces UP.'],
 ['Disable links','Set interfaces DOWN while keeping the devices.'],
 ['Detach ports','Remove retained interfaces from bridge membership.'],
 ['GETLINK queries','Read interface indices and check interface state.'],
 ['Batch deletion','Delete fresh veth/VXLAN devices in one batch.'],
 ['Conntrack deletes','Remove scoped connection-tracking and NAT entries.'],
 ['Neighbor dump','List IP-to-link-layer address mappings for cleanup.'],
 ['Peer routes','Route host traffic to remote endpoint/gateway IPs.'],
]
operation_table(f,[.075,.21,.85,.31],descriptions)
para(f,.185,'B = original bridge measurements; NB = Oct 8 bridge-free. Fresh B CPU is unfiltered; the other columns are filtered. Different trials; full rows and scope are on p10/p12. External consumers and sleeping lock waits are excluded.',size=9,width=108)
para(f,.12,'GETLINK: bridge fresh has six setup checks + one reset check; bridge-free fresh has five setup checks. Pools have four (B) / three (NB) readiness checks.',size=9,width=108)
para(f,.065,'Lifecycle control only; packet encryption and phase-barrier experiments are excluded.',size=8.5,width=108)
save(f)
# 6 lock chart
lock_groups=[('Device creation',lambda k:'create_' in k),('TC installation',lambda k:'tc_' in k),('Attach / enable',lambda k:k.startswith('setup/') and any(t in k for t in ['attach','enable'])),('Disable links',lambda k:k.startswith('teardown/disable')),('Detach ports',lambda k:'detach_' in k),('Batch deletion',lambda k:'delete_vxlan' in k),('Addresses',lambda k:'address' in k),('GETLINK queries',lambda k:'readiness' in k or 'lookup' in k or 'verify_retained' in k),('Peer routes',lambda k:False)]
lock_vals=[]
for name,pred in lock_groups:
 row=[]
 for dataset in ['fresh','pool']:
  for h in ['103','104']:
   rr=D[dataset][f'host-{h}'];rr=rr['rows'] if dataset=='fresh' else rr['locks']['256']['rows']
   row.append(sum(v['hold_ms_per_cycle'] for k,v in rr.items() if pred(k)))
 for lifecycle in ['unpooled','pooled']:
  for h in ['103','104']:
   row.append(sum(v['hold_ms_per_cycle'] for k,v in bf(h,lifecycle,'locks')['rows'].items() if k!='other_actor' and BC['family'](k)==name))
 lock_vals.append(row)
for j in range(4):
 expected=(D['fresh'][f'host-{["103","104"][j]}']['fixture_hold_ms_per_cycle'] if j<2 else D['pool'][f'host-{["103","104"][j-2]}']['locks']['256']['fixture_hold_ms_per_cycle'])
 assert abs(sum(row[j] for row in lock_vals)-expected)<1e-6
f=page('RTNL hold time by operation family','Fine request-correlated RTNL holds with filtering enabled, concurrency 256.','Sources: Oct 6 fine-lock summary; retained locks/256. All units: elapsed RTNL ms/cycle.')
heat(ax(f,[.37,.55,.53,.22]),[x[0] for x in lock_groups]+['Total'],['Fresh\nB VM1','Fresh\nB VM2','Pool\nB VM1','Pool\nB VM2','Fresh\nNB VM1','Fresh\nNB VM2','Pool\nNB VM1','Pool\nNB VM2'],lock_vals+[np.sum(lock_vals,axis=0).tolist()],vmax=np.max(lock_vals))
descriptions=[
 ['Device creation','Create the veth pair and VXLAN interface.'],
 ['TC installation','Add packet-hook attachment points and TC filters.'],
 ['Attach / enable','Join interfaces to the bridge and set interfaces UP.'],
 ['Disable links','Set interfaces DOWN while keeping the devices.'],
 ['Detach ports','Remove retained interfaces from bridge membership.'],
 ['Batch deletion','Delete fresh veth/VXLAN devices in one batch.'],
 ['Addresses','Add/remove endpoint IP addresses on interfaces.'],
 ['GETLINK queries','Read interface indices and check interface state.'],
 ['Peer routes','Route host traffic to remote endpoint/gateway IPs.'],
]
operation_table(f,[.075,.25,.85,.27],descriptions)
para(f,.225,'B = Oct 6 bridge traces; NB = Oct 8 bridge-free traces. NB RTNL totals: fresh 1.558 / 1.549 ms; pooled 0.530 / 0.503 ms. Lock-held elapsed time includes sleeps/preemption; it is not active CPU.',size=9,width=108)
para(f,.155,'Fresh: creation, deletion and VXLAN/bridge disable account for about 70% of RTNL; deletion holds 182 / 179 ms per 256-endpoint batch.',size=9,width=108)
para(f,.075,'Pooled: link changes account for about 91% of RTNL; detaching ports accounts for about 40%. No RTNL was observed for IPsec, conntrack or neighbor requests; those still consume CPU.',size=9,width=108)
save(f)
# 7 pooled operations
f=page('Pooled RTNL without bridges','Filtered C256 • Original bridge results plus bridge-free operation columns.','Sources: Oct 6 retained-cpu-lock; Oct 8 bridgefree-lifecycle summary. Complete attribution: p13.')
ops=[('Detach outer veth',['teardown/detach_outer'],[]),('Detach VXLAN',['teardown/detach_transport'],[]),('Disable container peer',['teardown/disable_container_peer'],['teardown/disable_container']),('Attach / enable outer',['setup/attach_and_enable_outer'],['setup/enable_outer']),('Disable VXLAN',['teardown/disable_transport'],['teardown/disable_transport']),('Disable bridge',['teardown/disable_bridge'],[]),('Attach / enable VXLAN',['setup/attach_and_enable_transport'],['setup/enable_transport']),('Enable bridge',['setup/enable_bridge'],[]),('Disable outer veth',[],['teardown/disable_outer']),('Add / remove peer routes',[],['setup/add_peer_ip_route','setup/add_peer_gateway_route','teardown/remove_peer_ip_route','teardown/remove_peer_gateway_route'])]
rows=[]
for name,old,new in ops:
 rows.append([name]+[f'{sum(pv(h,k) for k in old):.4f}' for h in ['103','104']]+[f"{sum(bf(h,'pooled','locks')['rows'].get(k,{}).get('hold_ms_per_cycle',0) for k in new):.4f}" for h in ['103','104']])
rows.append(['Total fixture RTNL']+[f"{D['pool'][f'host-{h}']['locks']['256']['fixture_hold_ms_per_cycle']:.4f}" for h in ['103','104']]+[f"{bf(h,'pooled','locks')['fixture_hold_ms_per_cycle']:.4f}" for h in ['103','104']])
table(f,[.075,.48,.85,.30],['Operation','B VM1','B VM2','NB VM1','NB VM2'],rows,[.48,.13,.13,.13,.13],9,heatmap=True)
label(f,.43,'Bridge membership disappears; link and route work remains','Fixed TC redirects replace bridge membership. Pooled activation and reset still enable/disable the dedicated devices, change addresses and peer routes, replace encryption state and clear stale state. Redirect installation is outside each lease.')
para(f,.265,'B = original Oct 6 bridge traces; NB = Oct 8 bridge-free traces. The new matched bridged reruns held RTNL 1.582 / 1.540 ms/cycle, versus 0.530 / 0.503 without bridges: a 66.5–67.3% reduction. Comparisons use the same historical cleanup scope.',size=10)
para(f,.15,'Rows above highlight the former hotspot and its replacement operations; p13 accounts for every measured operation. CPU and RTNL are separate overlapping budgets. Initial preparation and final pool destruction remain outside lease timing.',size=9.5)
save(f)
# 8 ceilings
f=page('Bridge-free budgets and achieved rates','Filtered C256 • Eight CPUs • Separate profiler-free and RTNL trials.','Source: Oct 8 bridgefree-lifecycle summary. CPU/RTNL ceilings are conditional, not measured rates.')
rows=[]
for lifecycle,name in [('unpooled','Fresh'),('pooled','Pooled')]:
 for h in ['103','104']:
  perf=bp(h,lifecycle); locks=bf(h,lifecycle,'locks')
  rows.append([name+' '+VM_LABELS[h],f"{perf['cycles_per_second']:.0f}",f"{perf['host_cpu_ms_per_cycle']:.3f}",f"{locks['fixture_hold_ms_per_cycle']:.3f}",f"{8000/perf['host_cpu_ms_per_cycle']:.0f}",f"{1000/locks['fixture_hold_ms_per_cycle']:.0f}"])
table(f,[.075,.635,.85,.145],['Bridge-free','Observed /s','CPU ms','RTNL ms','CPU cap /s','RTNL cap /s'],rows,[.23,.16,.15,.15,.16,.15],9)
label(f,.57,'Removing bridges lifts the serialized-work ceiling','Fresh bridge-free cycles still create/delete dedicated devices and hold RTNL about 1.55 ms/cycle. Pools retain those devices and hold about 0.50–0.53 ms/cycle, leaving link state and route changes as necessary work.')
para(f,.405,'Observed matched gains are 26–27% fresh and 79–81% pooled. Sixty-second C256 confirmations reach 507 / 509 cycles/s fresh and 875 / 881 pooled. The short repeated runs above reach 522 / 527 and 914 / 953; sustained confirmation does not establish 1,000/s.',size=10)
label(f,.28,'A resource ceiling is not a throughput promise','CPU cap = 8,000 / host CPU ms per cycle; RTNL cap = 1,000 / lock-held ms per cycle. These assume unchanged work and ideal utilization. Admission, scheduling, other lock users and completed-reset drains can keep throughput below either ceiling.')
para(f,.115,'The same historical reset scope is retained. Product-wide conntrack/address dumps, controller work and other integration costs are separate; these kernel rates are not product request or bulk packet throughput.',size=9.5)
save(f)
# 9 fresh CPU table
f=page('Fresh-device CPU: complete attribution','C256 • Bridge: unfiltered Oct 5 • Bridge-free: filtered Oct 8 • Calling-thread CPU ms/cycle','Sources: lifecycle-cpu-2026-10-05/summary.json; bridgefree-lifecycle-2026-10-08/summary.json.')
cr=[('SETUP · Veth creation',['setup/create_veth']),('VXLAN creation',['setup/create_vxlan']),('TC qdisc + three filters',['setup/tc_qdisc_add','setup/tc_filter_add']),('Two IPsec states + outbound policy',[k for k in cpu('103') if k.startswith('setup/IPsec')]),('Two addresses',['setup/address_add']),('Two attach + enable requests',['setup/enable_attach']),('Bridge + container peer enable',['setup/enable']),('Six setup GETLINK lookups/checks',['setup/getlink_readiness']),('RESET · Two disable requests',['teardown/disable']),('Remove IPsec states + policy',[k for k in cpu('103') if k.startswith('teardown/IPsec')]),('Acknowledged batch device deletion',['teardown/delete_link_batch']),('Four scoped conntrack deletions',['teardown/conntrack_delete']),('Remove bridge address',['teardown/address_remove']),('Bridge GETLINK check',['teardown/getlink_readiness'])]
rows=[[name]+[f'{sum(cpu(h)[k] for k in keys):.4f}' for h in ['103','104']] for name,keys in cr]
rows.append(['Synchronous request subtotal']+[f'{sum(sum(cpu(h)[k] for k in keys) for _,keys in cr):.4f}' for h in ['103','104']])
rows.append(['Neighbor dump · separate from requests']+[f'{cpu(h)["teardown/neighbor_dump_inclusive"]:.4f}' for h in ['103','104']])
rows.append(['Total measured operation CPU']+[f'{sum(cpu(h)[k] for _,keys in cr for k in keys)+cpu(h)["teardown/neighbor_dump_inclusive"]:.4f}' for h in ['103','104']])
rows=BC['extend'](rows,BF,'unpooled','cpu',BC['FRESH_CPU'])
table(f,[.075,.33,.85,.45],['Operation / combined calls','B VM1','B VM2','NB VM1','NB VM2'],rows,[.48,.13,.13,.13,.13],8,heatmap=True)
para(f,.265,'Original bridge CPU repeats covered 7,936 / 8,448 cycles. Bridge-free counts and integrity are in the Oct 8 JSON. Calling-thread CPU excludes sleeps and other execution owners. Original neighbor dumping is separate from request timers; overlapping inclusive timers are not added.',size=9.5)
para(f,.15,'B = original unfiltered Oct 5 bridge CPU; NB = filtered Oct 8 bridge-free CPU. Both use C256 and the historical reset scope. TC rows combine one qdisc/three filters (B) and two qdiscs/eight filters (NB). Host gateway addressing remains; bridge-free host routes and direct link enables are explicit.',size=9.5)
save(f)
# 10 fresh RTNL table
f=page('Fresh-device RTNL: complete attribution','Filtered C256 • Bridge: Oct 6 • Bridge-free: Oct 8 • RTNL hold ms per local cycle','Sources: Oct 6 fine-lock summary; Oct 8 bridgefree-lifecycle summary. Separate repeated traces.')
fr=[('SETUP · Create veth pair',['setup/create_veth']),('Look up container peer',['setup/creation_lookup_inner']),('Create VXLAN',['setup/create_vxlan']),('Look up VXLAN index',['setup/creation_lookup_transport']),('TC qdisc',['setup/tc_qdisc']),('Three TC filters',['setup/tc_filter']),('Two IPsec states + policy',['setup/ipsec_16','setup/ipsec_19']),('Add container address',['setup/add_container_address']),('Add bridge address',['setup/add_bridge_address']),('Attach outer veth + enable',['setup/attach_and_enable_outer']),('Attach VXLAN + enable',['setup/attach_and_enable_transport']),('Enable bridge',['setup/enable_bridge']),('Enable container peer',['setup/enable_container_peer']),('Readiness: VXLAN',['setup/readiness_transport']),('Readiness: outer veth',['setup/readiness_outer']),('Readiness: bridge',['setup/readiness_bridge']),('Readiness: container peer',['setup/readiness_inner']),('RESET · Disable VXLAN',['teardown/disable_transport']),('Remove IPsec states + policy',['teardown/ipsec_17','teardown/ipsec_20']),('Batch-delete VXLAN + veths',['teardown/delete_vxlan_and_veth_batch']),('Four scoped conntrack deletes',['teardown/scoped_conntrack_delete']),('Disable bridge',['teardown/disable_bridge']),('Dump bridge neighbors',['teardown/bridge_neighbor_dump']),('Remove bridge address',['teardown/remove_bridge_address']),('Verify bridge GETLINK',['teardown/verify_retained_bridge'])]
rows=[[name]+[f'{sum(fv(h,k) for k in keys):.4f}' for h in ['103','104']] for name,keys in fr]
for row_label,key in [('Setup subtotal','setup_hold_ms_per_cycle'),('Teardown subtotal','teardown_hold_ms_per_cycle'),('Total fixture RTNL hold time','fixture_hold_ms_per_cycle')]:rows.append([row_label]+[f'{D["fresh"][f"host-{h}"][key]:.4f}' for h in ['103','104']])
rows=BC['extend'](rows,BF,'unpooled','locks',BC['FRESH_RTNL'])
table(f,[.075,.225,.85,.555],['Operation / combined calls','B VM1','B VM2','NB VM1','NB VM2'],rows,[.48,.13,.13,.13,.13],7.5,heatmap=True)
para(f,.18,'Zero means no RTNL observed, not zero CPU. Batch deletion is divided by 256 endpoints. Other actors add 0.106 / 0.101 ms/cycle, excluded here. Fresh CPU attribution is on p10.',size=9.5)
para(f,.095,'B = original Oct 6 bridge traces; NB = Oct 8 bridge-free traces. Same C256 historical lifecycle scope, separate trials. Zero can mean an absent operation or no observed RTNL. Batch deletion implicitly removes fresh bridge-free gateway addresses and peer routes.',size=9)
save(f)
# 11–12 pooled CPU and RTNL tables
pr=[('ACTIVATE · Two IPsec states','setup/ipsec_16'),('Outbound IPsec policy','setup/ipsec_19'),('Add container address','setup/add_container_address'),('Enable container peer','setup/enable_container_peer'),('Add bridge address','setup/add_bridge_address'),('Attach outer veth + enable','setup/attach_and_enable_outer'),('Attach VXLAN + enable','setup/attach_and_enable_transport'),('Enable bridge','setup/enable_bridge'),('Enable transport again','setup/enable_transport'),('Readiness: VXLAN','setup/readiness_transport'),('Readiness: outer veth','setup/readiness_outer'),('Readiness: bridge','setup/readiness_bridge'),('Readiness: container peer','setup/readiness_inner'),('RESET · Disable VXLAN','teardown/disable_transport'),('Disable bridge','teardown/disable_bridge'),('Detach outer veth','teardown/detach_outer'),('Detach VXLAN','teardown/detach_transport'),('Disable container peer','teardown/disable_container_peer'),('Remove outbound IPsec policy','teardown/ipsec_20'),('Remove two IPsec states','teardown/ipsec_17'),('Four exact conntrack deletes','teardown/scoped_conntrack_delete'),('Dump container neighbors','teardown/container_neighbor_dump'),('Dump bridge neighbors','teardown/bridge_neighbor_dump'),('Remove container address','teardown/remove_container_address'),('Remove bridge address','teardown/remove_bridge_address')]
for kind,metric in [('cpu','CPU'),('locks','RTNL')]:
 f=page(f'Pooled {metric}: complete attribution',
        'Filtered C256. Milliseconds of calling-thread CPU per complete local cycle.' if kind=='cpu' else 'Filtered C256. Milliseconds holding RTNL per complete local cycle.',
        f'Sources: Oct 6 retained-cpu-lock {kind}/256; Oct 8 bridgefree-lifecycle {kind}/256.')
 rows=[[name]+[f'{pv(h,key,kind):.4f}' for h in ['103','104']] for name,key in pr]
 for prefix,label_name in [('setup/','Activation subtotal'),('teardown/','Reset subtotal')]:
  rows.append([label_name]+[f'{sum(pv(h,k,kind) for _,k in pr if k.startswith(prefix)):.4f}' for h in ['103','104']])
 rows.append([f'Total measured {metric}'+(' hold time' if kind=='locks' else '')]+[f'{sum(pv(h,k,kind) for _,k in pr):.4f}' for h in ['103','104']])
 for h in ['103','104']:
  expected=sum(v['cpu_ms_per_cycle'] for v in D['pool'][f'host-{h}']['cpu']['256']['rows'].values()) if kind=='cpu' else D['pool'][f'host-{h}']['locks']['256']['fixture_hold_ms_per_cycle']
  assert abs(sum(pv(h,k,kind) for _,k in pr)-expected)<1e-6
 rows=BC['extend'](rows,BF,'pooled',kind,BC['POOL'])
 table(f,[.075,.225,.85,.555],['Operation / combined calls','B VM1','B VM2','NB VM1','NB VM2'],rows,[.48,.13,.13,.13,.13],7.5,heatmap=True)
 if kind=='cpu':
  para(f,.18,'Original bridge repeats covered 13,824 / 14,080 cycles. CPU excludes sleeps, key derivation and argument/label construction before requests. Original instrumented process CPU is 3.25 / 3.18 ms/cycle. Bridge-free counts remain in the Oct 8 JSON.',size=9)
  para(f,.095,'B = Oct 6 bridge CPU; NB = Oct 8 bridge-free CPU. Filtered C256, same historical reset. Fixed pooled redirects are installed during preparation, not per lease. Gateway address, peer routes, direct link state and both root neighbor dumps are included.',size=9)
 else:
  para(f,.18,'Original bridge traces covered 12,288 cycles per host. Bridge-free counts and integrity remain in the Oct 8 JSON. Holds include sleeps/preemption while RTNL stays locked; acquisition waits are excluded. Zero means no observed RTNL, not zero CPU.',size=9)
  para(f,.095,'B = Oct 6 bridge traces; NB = Oct 8 bridge-free traces. Filtered C256, same historical reset. Direct enables/disables and route updates remain after bridge membership disappears. CPU and RTNL overlap and must not be added.',size=9)
 save(f)
pdf.close()
manifest={str(p.relative_to(ROOT)):hashlib.sha256(p.read_bytes()).hexdigest() for p in SOURCES.values()}
(HERE/'source-manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
print(f'{PDF}: {page_no} pages')
