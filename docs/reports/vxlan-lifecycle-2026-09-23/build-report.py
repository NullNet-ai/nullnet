"""Build reviewable tables and PDFs from the retained experimental records."""
import collections,csv,html,json,pathlib,re,statistics as st,tarfile,functools
from reportlab.lib import colors
from reportlab.lib.styles import getSampleStyleSheet,ParagraphStyle
from reportlab.lib.enums import TA_LEFT
from reportlab.lib.pagesizes import A4
from reportlab.platypus import SimpleDocTemplate,Paragraph,Spacer,Table,TableStyle,PageBreak,KeepTogether
from reportlab.graphics.shapes import Drawing,Rect,String
ROOT=pathlib.Path(__file__).resolve().parent
HOSTS=[103,104];MED=st.median
LABELS={'docker_pid_lookup':'Docker PID inspection','namespace_create':'Create named network namespace','endpoint_veth':'Create/move endpoint veth','namespace_address':'Namespace address (CLI)','endpoint_namespace':'Namespace link UP / native configuration','namespace_default_route':'Standalone default route','bridge_attach':'Gateway + dedicated bridge lease + endpoint attachment','stale_cross_host_cleanup':'Stale cross-host cleanup probe','stale_same_host_cleanup':'Stale same-host lookup probes','transport_veth':'Create/reuse encrypted transport veth','vxlan':'Create/configure dedicated VXLAN','key_salt_hash':'Key salt hashing subprocess','crypto_install':'Install MACsec / IPsec + mark filters','crypto_attach':'Attach protected interface','ip_forward':'Set IPv4 forwarding','forward_policy':'Set FORWARD policy'}
@functools.lru_cache(None)
def load(h,pattern):return [json.loads(p.read_text()) for p in sorted((ROOT/f'raw-{h}').glob(pattern)) if p.suffix=='.json']
def trials(h,where,mode,standalone=False):
 if where=='before':return load(h,('standalone-before-' if standalone else '')+'current-'+('same_macsec' if mode=='same' else 'unique_ipsec')+'-separate-current-*.json')
 return load(h,('dedicated-standalone-verified' if standalone else 'dedicated-verified')+'-native-'+('same_macsec' if mode=='same' else 'marked_ipsec')+'-batch-exact-*.json')
def med(ds,key):return MED(d.get(key,0) for d in ds)
def cold(d):return d['setup_seconds']+sum(d.get(k,0) for k in ['discovery_seconds','identity_init','pool_init','forwarding_init','security_init','shared_init'])-(d.get('discovery_seconds',0) if d.get('endpoint_type')=='standalone' else 0)
def shut(d):return d['teardown_seconds']+d.get('pool_finalize',0)
def fmt(x):return f'{x:.3f}'
def span(ds,f):return f'{MED(f(d) for d in ds):.3f} [{min(f(d) for d in ds):.3f}–{max(f(d) for d in ds):.3f}]'
def table(headers,rows):return '\n'.join(['| '+' | '.join(headers)+' |','| '+' | '.join(['---']*len(headers))+' |']+['| '+' | '.join(map(str,r))+' |' for r in rows])
def csvout(name,headers,rows):
 with (ROOT/name).open('w') as f:w=csv.writer(f);w.writerow(headers);w.writerows(rows)
styles=getSampleStyleSheet();styles['BodyText'].fontSize=9;styles['BodyText'].leading=12;styles['BodyText'].spaceAfter=7
styles.add(ParagraphStyle(name='Cell',fontSize=7.5,leading=10,spaceAfter=0));styles.add(ParagraphStyle(name='Small',fontSize=8,leading=11))
def markup(s):
 s=html.escape(s);s=re.sub(r'\[([^\]]+)\]\((https?[^)]+)\)',r'<a href="\2" color="#166084">\1</a>',s);s=re.sub(r'\*\*([^*]+)\*\*',r'<b>\1</b>',s);s=re.sub(r'`([^`]+)`',r'<font name="Courier">\1</font>',s);return s
class Chart(Drawing):
 def __init__(self,rows,title):
  super().__init__(490,45+24*len(rows));self.add(String(0,self.height-12,title,fontName='Helvetica-Bold',fontSize=10));maximum=max(v for _,v,_ in rows)
  for i,(label,val,color) in enumerate(rows):
   y=self.height-38-i*24;self.add(String(0,y,label,fontName='Helvetica',fontSize=8));self.add(Rect(155,y-3,max(1,260*val/maximum),12,fillColor=colors.HexColor(color),strokeColor=None));self.add(String(422,y,f'{val:.3f} s',fontName='Helvetica',fontSize=8))
def pdf(md,path,charts=None):
 story=[];lines=md.splitlines();i=0
 while i<len(lines):
  line=lines[i].strip();i+=1
  if not line:continue
  if line=='<!--pagebreak-->':story.append(PageBreak());continue
  if line.startswith('<!--chart:'):
   story.append(charts[line[10:-3]]);story.append(Spacer(1,12));continue
  if line.startswith('|'):
   raw=[line]
   while i<len(lines) and lines[i].startswith('|'):raw.append(lines[i]);i+=1
   rows=[[x.strip() for x in r.strip('|').split('|')] for r in raw if not re.match(r'^\|[\s:|-]+\|$',r)]
   n=len(rows[0]);widths=([230]+[(490-230)/(n-1)]*(n-1)) if n<5 else ([158]+[(490-158)/(n-1)]*(n-1))
   t=Table([[Paragraph(markup(c),styles['Cell']) for c in row] for row in rows],colWidths=widths,repeatRows=1,hAlign='LEFT')
   t.setStyle(TableStyle([('BACKGROUND',(0,0),(-1,0),colors.HexColor('#e9eef2')),('VALIGN',(0,0),(-1,-1),'TOP'),('LINEBELOW',(0,0),(-1,0),.5,colors.HexColor('#8a9aa5')),('ROWBACKGROUNDS',(0,1),(-1,-1),[colors.white,colors.HexColor('#f6f8fa')]),('LEFTPADDING',(0,0),(-1,-1),5),('RIGHTPADDING',(0,0),(-1,-1),5),('TOPPADDING',(0,0),(-1,-1),4),('BOTTOMPADDING',(0,0),(-1,-1),4)]));story.extend([t,Spacer(1,12)]);continue
  if line.startswith('#'):
   depth=len(line)-len(line.lstrip('#'));style='Title' if depth==1 else 'Heading'+str(min(depth-1,3));story.append(Paragraph(markup(line.lstrip('# ')),styles[style]));continue
  if line.startswith('- '):line='• '+line[2:]
  story.append(Paragraph(markup(line),styles['BodyText']))
 def footer(c,d):
  c.setFont('Helvetica',8);c.setFillColor(colors.HexColor('#65727d'));c.drawString(45,27,'Nullnet lifecycle experiments • 23 September 2026');c.drawRightString(A4[0]-45,27,str(d.page))
 SimpleDocTemplate(str(path),pagesize=A4,rightMargin=45,leftMargin=45,topMargin=42,bottomMargin=45,title=md.splitlines()[0].lstrip('# '),author='Nullnet performance investigation').build(story,onFirstPage=footer,onLaterPages=footer)

parts=['# Nullnet encrypted endpoint lifecycle: before and after','23 September 2026 · Linux 6.12.95+deb13-amd64 · hosts 192.168.1.103 and .104. Each host: 8 vCPUs, about 16 GiB RAM, KVM, AMD Ryzen 7 7840HS.',
'''The selected experimental design shares encrypted VXLAN sockets and reuses fully reset dedicated bridges. It keeps the current one-bridge-per-endpoint forwarding topology; bridge-netfilter settings stay unchanged. Same-host keeps per-edge MACsec. Cross-host keeps per-edge IPsec SAs and encrypted VXLAN metadata, using authenticated marks to share one UDP port. The selected cross-host variant uses unique request IDs, a shared inbound selector policy, per-edge outbound policies and a 4,096-packet replay window. A bounded pool holds anonymous, down, address-free dedicated bridges between leases. Every active endpoint still has its own bridge, MACsec or VXLAN, addresses and independent keys. Pool reset is included in endpoint teardown. Destroying the entire empty pool remains about 20 seconds per 1,000 bridges and is reported separately; it is not a solved fast-destruction path.''',
'''**Result boundary.** These are measured complete kernel endpoint sequences in a source-matched Python/Netlink harness, with real Docker namespaces and separate two-host packet proofs. They are not measurements of the full Nullnet RPC/database/eBPF/control-plane path. Product code was not changed. This establishes a tested implementation candidate; it does not establish production equivalence for every Nullnet recovery, routing or security path.''',
'''**Unit.** Every lifecycle trial has 1,000 endpoint halves on one host. Same-host therefore creates 500 encrypted pairs. A cross-host edge requires one endpoint on each host; timings from two isolated endpoint trials cannot be added or relabeled as measured complete distributed edges/s. The headline is comparable to the earlier 600+ bare-VXLAN endpoints/s measurement, not 1,000 full Nullnet networks/s.''',
'## Four-case result: Docker endpoints','Median seconds per 1,000 endpoints, three repeats per host. “Cold setup” includes batched discovery, pinned identity acquisition, host settings, bridge creation and crypto guards. “Final shutdown” also destroys the empty shared bridges.']
headers=['Case / host','Before full','After endpoints','After cold / final','After endpoint rate/s'];rows=[];summary=[];charts={}
for mode in ['same','cross']:
 for phase in ['setup','teardown']:
  for h in HOSTS:
   a=trials(h,'after',mode);b=trials(h,'before',mode);key=phase+'_seconds';extra=cold if phase=='setup' else shut
   rows.append([f'{mode}-host {phase} · {h}',fmt(med(b,key)),fmt(med(a,key)),fmt(MED(extra(d) for d in a)),f'{1000/med(a,key):.0f}'])
   summary.append([mode,phase,h,med(b,key),med(a,key),MED(extra(d) for d in a),1000/med(a,key),med(b,key)/med(a,key)])
parts.append(table(headers,rows));csvout('sequence-summary.csv',['topology','phase','host','before_seconds','after_endpoint_seconds','after_cold_or_final_seconds','after_endpoints_per_second','speedup'],summary)
parts+=['The approximately 1,000/s stretch target is approached for warm teardown, not uniformly for setup or complete cold lifecycle. Setup is around the earlier 600+ endpoints/s class. Count cold startup and final shutdown when that is the workload; keeping empty shared infrastructure is an explicit operating choice, not omitted endpoint cleanup.',
'<!--pagebreak-->','## Measurement and correctness boundaries',
'''The before path reproduces current per-endpoint Docker inspection, namespace address/link subprocesses, native veth/bridge/VXLAN operations, stale-path probes, four crypto commands, salt hashing for IPsec, forwarding sysctl and FORWARD-policy updates. Teardown includes crypto cleanup, a link inventory, ownership regrouping and acknowledged grouped link deletion. Fixtures start clean: stale-path probes find nothing. Omitting repeated stale probes in the after path depends on authoritative lifecycle ownership and startup reconciliation; this benchmark does not establish the cost or correctness of migrating an already-dirty production host. It models current cleanup at 128 endpoints per idle batch. Same-host takes the complete per-edge lifecycle lock, including both halves' shared transport creation.''',
'''The selected after path keeps 32 bounded workers and eight CLI slots, namespace-bound Netlink sockets, native MACsec or XFRM/TC, one startup forwarding transaction, dedicated bridge leases and 256-endpoint idle deletion batches. Docker identity acquisition pins a network namespace FD and pidfd for each of 12 benchmark containers and checks process death before and after setup. Cross-host uses reserved UDP 4790; using a broad fail-closed rule on Docker Swarm's 4789 would be unsafe. The experiment uses dummy keys and addresses and does not read production keys.''',
'''Timing excludes creation of the 12 Docker fixtures, creating the isolated outer test namespace, debug inventories and post-run packet checks. It includes creation/removal of the endpoint's own objects. Standalone trials, where present, include each named endpoint namespace and default route. The steady setup clock starts after shared infrastructure is initialized; cold accounting adds all recorded initialization. Initializing the MACsec generic-Netlink family and interpreter overhead are not included in the cold formula; they are not production startup measurements.''',
'''Setup step tables report average elapsed latency spent inside each step per endpoint, including semaphore/RTNL waits. Parallel endpoint work overlaps: these rows must NOT be summed to get burst wall time. Full burst wall time is separately measured. Micro-operation tables separate subprocess execution from waiting for one of eight CLI slots. Teardown phases are sequential wall intervals and can be added; residual records scheduling, selection, assertions and loop overhead. These tables are not isolated single-operation capacity tests.''',
'''Network geometry: performance fixtures use an isolated dummy underlay and /30 addresses to measure lifecycle cost. Packet proofs use the real two-host underlay and /24 overlay subnets; same-host packet tests additionally exercise the production-style shared /29 with two endpoint/gateway addresses. Therefore 1,000 endpoints are not claimed to carry simultaneous end-to-end traffic during the throughput trial. MTU tests use endpoint MTU 1080. MACsec transport MTU in this harness is 1120.''',
'''Three repetitions characterize repeatability on these two hosts; they are not confidence intervals or a universal capacity promise. Python instrumentation and the interpreter contribute overhead. A Rust implementation may differ in either direction. Before and selected-after trials both use 32 workers / eight CLI slots. This is a native-configuration and topology comparison, not a one-line causal A/B.''']
step_csv=[];micro_csv=[]
for mode in ['same','cross']:
 parts+=['<!--pagebreak-->',f'## {mode.capitalize()}-host setup: step breakdown']
 keys=[]
 for where in ['before','after']:
  for e in trials(103,where,mode)[0]['endpoints']:
   for k in e['stages']:
    if k not in keys:keys.append(k)
 rows=[]
 for k in keys:
  vals=[]
  for where in ['before','after']:
   for h in HOSTS:
    ds=trials(h,where,mode);v=MED(sum(e['stages'].get(k,0) for e in d['endpoints'])/d['n']*1000 for d in ds);vals.append(f'{v:.3f}' if v else '—');step_csv.append([mode,'setup',where,h,k,v,'mean step latency ms per endpoint'])
  rows.append([LABELS.get(k,k),*vals])
 parts+=[table(['Step (ms per endpoint)','Before 103','Before 104','After 103','After 104'],rows),'Rows include waiting and overlap across endpoints. A dash means the work was removed, moved to initialization, or combined in another native step; it does not mean equivalent behavior may be skipped.']
 chartrows=[]
 for where,col in [('before','#d87459'),('after','#248c9b')]:
  for h in HOSTS:chartrows.append((f'{where.capitalize()} · host {h}',med(trials(h,where,mode),'setup_seconds'),col))
 charts[mode+'setup']=Chart(chartrows,'Measured full setup burst: 1,000 endpoints');parts.append('<!--chart:'+mode+'setup-->')
 parts.append(table(['Full setup seconds','Before','After warm','After cold'],[[str(h),span(trials(h,'before',mode),lambda d:d['setup_seconds']),span(trials(h,'after',mode),lambda d:d['setup_seconds']),span(trials(h,'after',mode),cold)] for h in HOSTS]))
 parts+=['<!--pagebreak-->',f'## {mode.capitalize()}-host teardown: step breakdown','Seconds per complete 1,000-endpoint burst; median of three measurements.']
 phases=['crypto_remove','link_dump','regroup','link_delete','last_peer_crypto','bridge_reset','namespace_remove'];rows=[]
 for k in phases+['residual','endpoint_total','pool_finalize','final_total']:
  vals=[]
  for where in ['before','after']:
   for h in HOSTS:
    ds=trials(h,where,mode)
    def value(d):
     if k=='residual':return d['teardown_seconds']-sum(d['teardown'].values())
     if k=='endpoint_total':return d['teardown_seconds']
     if k=='final_total':return shut(d)
     if k=='pool_finalize':return d.get(k,0)
     return d['teardown'].get(k,0)
    v=MED(value(d) for d in ds);vals.append(fmt(v));step_csv.append([mode,'teardown',where,h,k,v,'phase wall seconds per 1000 endpoints'])
  rows.append([{'crypto_remove':'Crypto/stale state cleanup','link_dump':'Owned-link inventory','regroup':'Assign retiring device group','link_delete':'Delete grouped links (ACK)','last_peer_crypto':'Remove final shared inbound policy','bridge_reset':'Reset bridge: DOWN, anonymous name, addresses/neighbors removed','namespace_remove':'Remove standalone namespaces','residual':'Other measured loop/check work','endpoint_total':'FULL ENDPOINT TEARDOWN','pool_finalize':'Destroy complete empty bridge pool','final_total':'FULL FINAL SHUTDOWN'}.get(k,k),*vals])
 parts.append(table(['Phase (seconds)','Before 103','Before 104','After 103','After 104'],rows));parts.append('Medians of individual phases need not sum exactly to the median total. Crypto deletion is bounded concurrent work within its measured wall interval. No IPsec state is needed on the same-host MACsec path; the current stale-state probe still incurs process cost.')
 chartrows=[]
 for where,col in [('before','#d87459'),('after','#248c9b')]:
  for h in HOSTS:chartrows.append((f'{where.capitalize()} · host {h}',med(trials(h,where,mode),'teardown_seconds'),col))
 charts[mode+'teardown']=Chart(chartrows,'Measured full endpoint teardown: 1,000 endpoints');parts.append('<!--chart:'+mode+'teardown-->')
parts+=['<!--pagebreak-->','## Shared infrastructure and cold costs','Seconds added outside the warm endpoint setup timer. Per-container discovery is batched; namespace/pid FDs are held for the generation.']
rows=[]
for k in ['discovery_seconds','identity_init','pool_init','forwarding_init','security_init','shared_init','pool_finalize']:
 rows.append([k,*[fmt(med(trials(h,'after',mode),k)) for mode in ['same','cross'] for h in HOSTS]])
parts.append(table(['Initialization / shutdown','Same 103','Same 104','Cross 103','Cross 104'],rows))
parts+=['## Why the teardown was slow',
'''The current topology creates one bridge per endpoint. Function-graph tracing and the Linux bridge source show a serial RCU barrier in bridge multicast destruction even when multicast snooping is disabled. This accounts for the roughly 21-second same-host teardown. Cross-host teardown adds a last-user UDP tunnel socket release for each unique destination port, producing roughly another 20 seconds. Replacing only IPsec or sharing only the VXLAN port leaves the bridge bottleneck.''',
'''A bounded pool of clean dedicated bridges removes bridge destruction from endpoint retirement; using marked IPsec lets distinct encrypted edges share a compatible UDP socket. The last shared socket and final bridge destructors still cost time and are explicitly included when the entire pool is removed.''']
for h in HOSTS:
 f=ROOT/f'raw-{h}'/'trace-results.json'
 if f.exists():parts.append(table([f'Host {h}: traced component deletion','Count','Seconds'],[[d['mode'],d['n'],fmt(d['delete_seconds'])] for d in json.loads(f.read_text())]))
parts+=['Traced component runs use 16 objects, not 1,000; tracing is attribution evidence and adds overhead. Do not extrapolate their exact duration linearly or add inclusive nested function times.',
'[Bridge destructor source](https://github.com/gregkh/linux/blob/v6.12.95/net/bridge/br_multicast.c) · [VXLAN socket lifecycle](https://github.com/gregkh/linux/blob/v6.12.95/drivers/net/vxlan/vxlan_core.c)',
'<!--pagebreak-->','## Concurrent churn, isolation and reuse',
'''Churn maintains 512 live endpoints, concurrently retires 256 and creates 256, and repeats six waves. IDs circulate through 1,024 slots with fresh incarnation keys. Each round checks exact device and crypto counts. Final checks require zero container peers and only clean anonymous bridge slots, without addresses, neighbors, dynamic FDB entries or multicast memberships. Both hosts and both selected encryption modes passed. This is mixed setup/teardown load, not 1,000/s independently for each direction.''']
rows=[]
for h in HOSTS:
 path=ROOT/f'raw-{h}'/'churn-dedicated.json'
 if not path.exists():continue
 for r in json.loads(path.read_text()):
  es=[e for x in r['rounds'] for e in x['endpoints']];lat=sorted(e['seconds'] for e in es);comp=sorted(e.get('completion',0) for e in es);wall=st.mean(x['wall'] for x in r['rounds']);dels=[v.get('delete',0) if isinstance(v,dict) else 0 for x in r['rounds'] for v in x['delete']['batch_wall']]
  rows.append([f'{h} / {r["mode"]} / {r["batch"]}',f'{512/wall:.0f}',f'{lat[int(.99*len(lat))]*1000:.1f}',f'{comp[int(.99*len(comp))]*1000:.1f}' if any(comp) else 'not recorded',f'{max(dels)*1000:.1f}' if any(dels) else 'not recorded'])
parts.append(table(['Host / mode / batch','Mixed ops/s','p99 active ms','p99 completion ms','Max delete ACK ms'],rows))
parts+=['Active latency starts inside the worker; completion includes executor admission from the burst start. Delete-ACK duration is a userspace call duration, not a direct RTNL hold-time measurement. With 256 creates and 256 deletes per wave, each operation direction is half the mixed rate. The 256 idle batch is not automatically a safe replacement for the product’s smaller batch while setups are active; retain bounded adaptive admission until integrated load testing establishes a tail-latency budget.',
'''Bridge MACs are explicitly pinned and independently generated across hosts. Bridge reuse preserves the dedicated forwarding topology and the existing bridge-netfilter settings.''',
'[Bridge MAC recalculation source](https://github.com/gregkh/linux/blob/v6.12.95/net/bridge/br_stp_if.c)',
'<!--pagebreak-->','## Packet and lifecycle correctness evidence',
'''The final marked-IPsec proof runs 32 Docker-backed edges on the two real hosts. Both directions pass all 32 connectivity tests; host gateway reachability and an unfragmented 1080-byte endpoint MTU pass. The dedicated-bridge candidate passes exact preservation of eight Ethernet frame formats, including customer tags, nested tags and priority bits, and the routed source-policy/SNAT/FORWARD-rule comparison under unchanged bridge-netfilter settings. Tagged-frame injection includes valid delivery controls; foreign and double-tagged probes deliver no marker to another endpoint. This checks carried Ethernet traffic, not a VLAN-based replacement for VXLAN.''',
'''The key negative test deliberately sends VNI 1 through edge 0's valid outbound SA. The receiving TC filter rejects the authenticated but mismatched edge mark; edge 0 survives, and restoring the correct mark restores edge 1. Independent wrong-key and missing-all-crypto tests fail closed. ESP replay beyond the selected 4,096-packet window increments XfrmInStateSeqError. Three fresh-key delete/recreate cycles retain connectivity and an unrelated edge survives each deletion.''',
'''Same-host tests exercise 64 endpoint halves / 32 pairs, bidirectional traffic, the shared /29 layout, MTU, TX-SA disable/restore, five fresh-key identifier/bridge/SCI reuse cycles and a real benchmark-container restart. The pinned old process generation is rejected before creating its veth; refreshing the namespace/pid FDs allows setup to recover. These passed on both hosts.''']
for name in ['dedicated-marked-proof.json']:
 p=ROOT/name
 if p.exists():
  d=json.loads(p.read_text());parts.append(f'Evidence file: `{name}`. Connectivity success counts: '+str([x['success'] for x in d['connectivity']])+'.')
rows=[]
for h in HOSTS:
 p=ROOT/f'raw-{h}'/'partial-failure-dedicated.json'
 if p.exists():
  for r in json.loads(p.read_text()):rows.append([str(h),r['mode'],'Injected failure → owned cleanup → retry passed' if r['retry_success'] else 'FAILED'])
if rows:parts+=['## Partial setup failure',table(['Host','Mode','Observed result'],rows),'The harness injects failure after partial crypto configuration, removes only the failed edge’s objects, verifies unchanged survivor counts, and successfully reuses the slot. This proves the cleanup recipe, not automatic rollback in the unmodified Nullnet client.']
parts+=['## Dataplane throughput check','Short single-edge iperf3 tests (5 seconds, one/eight TCP streams, both directions). These are sanity checks, not a sustained multicore or many-edge capacity claim.']
rows=[];traffic_rows=[]
for reverse in [False,True]:
 for streams in [1,8]:
  vals={}
  for mode in ['current_docker','dedicated_marked']:
   vals[mode]=[]
   for f in sorted(ROOT.glob('traffic-dedicated-repeat-*'+mode+'.json')):
    d=json.loads(f.read_text())
    for r in d['throughput']:
     if r['reverse']==reverse and r['streams']==streams:
      v=r['result']['end']['sum_received']['bits_per_second']/1e9;vals[mode].append(v);traffic_rows.append([f.name,mode,reverse,streams,v,r['result']['end'].get('sum_sent',{}).get('retransmits',0)])
  if all(vals.values()):
   before=MED(vals['current_docker']);after=MED(vals['dedicated_marked']);rows.append([('reverse' if reverse else 'forward')+f' / {streams} streams',f'{before:.3f} [{min(vals["current_docker"]):.3f}–{max(vals["current_docker"]):.3f}]',f'{after:.3f} [{min(vals["dedicated_marked"]):.3f}–{max(vals["dedicated_marked"]):.3f}]',f'{(after/before-1)*100:.1f}%'])
parts.append(table(['Direction / streams','Current Gbit/s','Marked Gbit/s','Median change'],rows))
csvout('dataplane-trials.csv',['file','mode','reverse','streams','receive_gbit_s','retransmits'],traffic_rows)
parts.append('Three repetitions per path, alternating order; cells are median [min–max]. The selected candidate’s measured change is shown above; these short samples do not establish zero dataplane regression. All six repeated 100-byte UDP tests at 10 Mbit/s had zero loss. These repeated samples use the final shared-inbound, unique-reqid, 4,096-window configuration with 32 installed edges. The separate 1,000-edge check below exercises a larger policy table; sustained traffic on all edges simultaneously remains an integration requirement.')
p=ROOT/'scale-dedicated-proof.json'
if p.exists():
 parts+=['<!--pagebreak-->','## Final combined cross-host configuration','The final design keeps per-edge SAs/keys and outbound marked policies, shares only the peer/port inbound policy, gives each edge a unique reqid, and enables a 4,096-packet replay window. At 1,000 edges this uses 2,000 independent SAs and 1,001 policies per host. Its separate full security proof rechecks old-frame rejection after advancing beyond that window, wrong SA/VNI, wrong key, missing crypto and fresh-key reuse.']
 rows=[]
 for d in json.loads(p.read_text()):
  for t in d['traffic']:rows.append([f'{t["edge"]} / '+('reverse' if t['reverse'] else 'forward')+f' / {t["streams"]}',f'{t["result"]["end"]["sum_received"]["bits_per_second"]/1e9:.3f}',str(t['result']['end']['sum_sent'].get('retransmits',0))])
 parts.append(table(['Edge / direction / streams','Final Gbit/s','TCP retransmits'],rows))
 for d in json.loads(p.read_text()):
  counters=[dict(line.split() for line in text.splitlines()) for text in d['xfrm_stats']];parts.append('Final cumulative XFRM counters on hosts 103/104: sequence rejects '+str([x['XfrmInStateSeqError'] for x in counters])+', missing states '+str([x['XfrmInNoStates'] for x in counters])+'. Counters cover installation and traffic.')
  if 'xfrm_phases' in d:
   phase_rows=[]
   for phase,values in [*d['xfrm_phases'].items(),('after_traffic',d['xfrm_stats'])]:
    c=[dict(line.split() for line in text.splitlines()) for text in values];phase_rows.append([phase,*[x['XfrmInNoStates'] for x in c],*[x['XfrmInStateSeqError'] for x in c]])
   parts.append(table(['Counter snapshot','Missing SA 103','Missing SA 104','Sequence 103','Sequence 104'],phase_rows))
 parts.append('Missing-SA counts above arise during concurrent installation, before both hosts finish setup. The post-install count remains unchanged through all 2,000 pings and eight TCP samples, with no sequence errors. Product publication must wait for both endpoint acknowledgements. Full 1,000-edge connectivity passed in both directions. Residual edge-position-dependent throughput and TCP retransmissions are reported rather than hidden; this is not a claim of constant dataplane cost at arbitrary scale.')
# Standalone numbers are included when the full trials have completed.
if trials(103,'after','same',True):
 parts+=['<!--pagebreak-->','## Standalone namespaces: complete additional sequences','These trials create and remove one named network namespace per endpoint and install its default route. Endpoint namespace lifecycle is included, unlike the Docker case. Gateway ping and default-route checks run after timing; namespace paths must be absent after cleanup. Three repeats, 1,000 endpoints per host.']
 rows=[]
 for mode in ['same','cross']:
  for h in HOSTS:
   a=trials(h,'after',mode,True);b=trials(h,'before',mode,True)
   if a:rows.append([f'{mode} / {h}',fmt(med(b,'setup_seconds')) if b else 'pending',fmt(MED(cold(d) for d in a)),fmt(med(b,'teardown_seconds')) if b else 'pending',fmt(med(a,'teardown_seconds')),fmt(MED(shut(d) for d in a))])
 parts.append(table(['Topology / host','Before setup s','After cold s','Before teardown s','After endpoint teardown s','After final s'],rows))
 for mode in ['same','cross']:
  rows=[]
  for where in ['before','after']:
   for h in HOSTS:
    ds=trials(h,where,mode,True)
    if ds:rows.append([f'{where} / {h}',fmt(MED(sum(e['stages'].get('namespace_create',0) for e in d['endpoints'])/d['n']*1000 for d in ds)),fmt(MED(d['teardown'].get('namespace_remove',0) for d in ds))])
  parts.append(table([mode+'-host namespace contribution','Create mean ms/endpoint','Remove full wall s'],rows))
 parts.append('Standalone remains below 600 endpoints/s in these complete sequences. The selected standalone candidate also replaces ip netns add/delete with dedicated-thread unshare, bind mount, namespace restore, unmount and unlink. It creates fresh namespaces; it does not recycle a previously used isolation domain. Removing those processes reduces deletion overhead, but fresh namespace creation and cross-namespace device cleanup still cost enough to miss the target. Do not substitute the faster Docker numbers for this full sequence.')
if trials(103,'after','same',True):
 for mode in ['same','cross']:
  parts+=['<!--pagebreak-->',f'## Standalone {mode}-host: every setup step','Mean elapsed milliseconds per endpoint, including waits. These concurrent intervals do not add to burst wall time.']
  keys=[]
  for where in ['before','after']:
   for e in trials(103,where,mode,True)[0]['endpoints']:
    for k in e['stages']:
     if k not in keys:keys.append(k)
  rows=[]
  for k in keys:
   vals=[]
   for where in ['before','after']:
    for h in HOSTS:
     ds=trials(h,where,mode,True);v=MED(sum(e['stages'].get(k,0) for e in d['endpoints'])/d['n']*1000 for d in ds);vals.append(f'{v:.3f}');step_csv.append(['standalone_'+mode,'setup',where,h,k,v,'mean step latency ms per endpoint'])
   rows.append([LABELS.get(k,k),*vals])
  parts.append(table(['Step (ms/endpoint)','Before 103','Before 104','After 103','After 104'],rows))
  parts+=[f'### Standalone {mode}-host: every teardown phase','Sequential wall seconds per 1,000 endpoints.']
  rows=[]
  for k in ['crypto_remove','link_dump','regroup','link_delete','last_peer_crypto','bridge_reset','namespace_remove','residual','endpoint_total','pool_finalize','final_total']:
   vals=[]
   for where in ['before','after']:
    for h in HOSTS:
     ds=trials(h,where,mode,True)
     def value(d):
      if k=='residual':return d['teardown_seconds']-sum(d['teardown'].values())
      if k=='endpoint_total':return d['teardown_seconds']
      if k=='final_total':return shut(d)
      if k=='pool_finalize':return d.get(k,0)
      return d['teardown'].get(k,0)
     v=MED(value(d) for d in ds);vals.append(fmt(v));step_csv.append(['standalone_'+mode,'teardown',where,h,k,v,'phase wall seconds per 1000 endpoints'])
   rows.append([k,*vals])
  parts.append(table(['Phase (seconds)','Before 103','Before 104','After 103','After 104'],rows))
parts+=['<!--pagebreak-->','## Candidate choice and remaining product work',
'''Select a bounded pool of reset dedicated bridges with stable, host-unique MACs; native namespace and crypto configuration; generation-bound Docker namespace handles; initialization/reconciliation of global forwarding; and shared-port per-edge marked IPsec cross-host. Same-host retains MACsec. Attach a protected transport to its dedicated bridge only after crypto and inbound identity filters are installed. Return a bridge lease only after acknowledged link removal and full reset. Keep pool destruction separate and explicit; it still exceeds the requested rate.''',
'''The endpoint candidate passed the packet and lifecycle checks described above. The remaining work is product integration and its four project gates: authoritative Docker generation/event reconciliation, cancellation and crash recovery, adoption of legacy bridges into the pool, per-edge locks, ownership-aware purge/discovery, lease generations and named bridge routes, ciphertext-only eBPF admission, reserved-port and mark allocation, firewall reload repair, key rotation/nonce lifetime, and real encrypted cold/warm concurrent Nullnet load including /api/graph state. No full CI or product multi-host regression claim is made for these report-only prototype files.''',
'''The final read-only audit on both hosts also found unchanged original link identities/MTUs/MACs and routes, zero benchmark containers/namespaces and zero temporary test-peer map entries. The existing application containers were left running. Test suites assert unchanged original Docker PID/start/network state. Packet work temporarily admits only the two isolated benchmark underlay IPs through the existing known-peer map and removes those exact entries afterward. No production firewall flush, application restart, product commit or deployment was performed.''',
'''The proposal-by-proposal rationale is in change-rationale.md / change-rationale.pdf, with individual documents in changes/. It states what changes, why equivalence is plausible, which tests passed, and which implementation obligations remain. A passing packet test is not a proof of every possible security property.''',
'## Retained evidence and reproducibility',
'''Raw trial JSON and traces are retained per host in compressed archives. sequence-summary.csv, step-breakdown.csv, operation-breakdown.csv and all-trials.csv expose the numeric data. lifecycle.py and native_*.py are Linux experimental harnesses, not product libraries. run-suite.py creates only labeled benchmark containers and namespaces; traffic-node.py and the proof coordinators require the documented lab topology and temporary exact peer-map admission. Do not run their hardcoded lab credentials or addresses on another environment. Use dedicated-verified-* for selected Docker results and dedicated-standalone-verified-* for standalone results.''']
# Full low-level operation appendix and machine-readable columns.
parts+=['<!--pagebreak-->','## Appendix: low-level setup operations','For each operation, values are mean active call milliseconds per endpoint, then median across three runs. Netlink calls include kernel/RTNL wait. CLI execution excludes semaphore queue time; a separate queue total follows. Socket setup, hashing in Python and scheduler overhead live in the enclosing step totals.']
for mode in ['same','cross']:
 for where in ['before','after']:
  perhost={};counts={}
  for h in HOSTS:
   pertrial=[];counttrial=[]
   for d in trials(h,where,mode):
    sums=collections.defaultdict(float);cnt=collections.Counter()
    for e in d['endpoints']:
     for op in e['operations']:sums[op['step']]+=op['seconds'];cnt[op['step']]+=1
     for c in e['commands']:
      a=c['argv'];label='CLI '+' '.join(a[:min(4,len(a))]);label=re.sub(r'nsenter -t \d+ -n','nsenter (target namespace)',label)
      if a[:3]==['ip','macsec','add']:label='CLI MACsec '+('TX SA' if 'tx' in a else 'RX SA' if 'sa' in a else 'RX channel')
      elif a[:3]==['ip','link','add']:label='CLI MACsec device create'
      elif a[0]=='docker':label='CLI docker inspect'
      elif a[0]=='nsenter':label='CLI namespace '+('address' if 'addr' in a else 'link UP' if 'link' in a else 'route')
      sums[label]+=c['execute'];cnt[label]+=1;sums['CLI semaphore queue (all commands)']+=c['queue']
    pertrial.append({k:1000*v/d['n'] for k,v in sums.items()});counttrial.append({k:v/d['n'] for k,v in cnt.items()})
   keys=set().union(*(x.keys() for x in pertrial));perhost[h]={k:MED(x.get(k,0) for x in pertrial) for k in keys};counts[h]={k:MED(x.get(k,0) for x in counttrial) for k in keys}
  keys=sorted(set(perhost[103])|set(perhost[104]));rows=[]
  for k in keys:
   rows.append([k,f'{counts[103].get(k,0):g}',f'{perhost[103].get(k,0):.3f}',f'{perhost[104].get(k,0):.3f}'])
   for h in HOSTS:micro_csv.append([mode,where,h,k,counts[h].get(k,0),perhost[h].get(k,0)])
  parts+=[f'### {mode.capitalize()}-host {where}',table(['Operation','Calls/endpoint','Host 103 ms','Host 104 ms'],rows)]
csvout('step-breakdown.csv',['topology','phase','version','host','step','value','unit'],step_csv);csvout('operation-breakdown.csv',['topology','version','host','operation','calls_per_endpoint','mean_elapsed_ms_per_endpoint'],micro_csv)
allrows=[]
for h in HOSTS:
 for p in sorted((ROOT/f'raw-{h}').glob('*.json')):
  try:d=json.loads(p.read_text())
  except ValueError:continue
  if not isinstance(d,dict) or 'setup_seconds' not in d:continue
  allrows.append([h,p.name,d.get('n'),d.get('mode'),d.get('endpoint_type','docker'),d.get('workers'),d.get('delete_batch'),d.get('bridge_pool',False),d['setup_seconds'],d['teardown_seconds'],d.get('pool_finalize',0),len(d.get('errors',[]))])
csvout('all-trials.csv',['host','file','n','mode','endpoint_type','workers','delete_batch','bridge_pool','setup_seconds','teardown_seconds','pool_finalize_seconds','errors'],allrows)
md='\n\n'.join(parts)+'\n';(ROOT/'analysis.md').write_text(md);pdf(md,ROOT/'nullnet-setup-teardown-before-after.pdf',charts)
rationale=(ROOT/'change-rationale.md').read_text();pdf(rationale,ROOT/'change-rationale.pdf')
changes=ROOT/'changes';changes.mkdir(exist_ok=True)
for previous in changes.glob('[0-9][0-9]-*.md'):previous.unlink()
names=['dedicated-bridge-pool','shared-vxlan-port','per-edge-marked-ipsec','native-macsec','namespace-sockets-and-container-identity','forwarding-initialization','bounded-deletion-batches','native-xfrm-and-tc','native-named-namespaces','shared-inbound-ipsec-policy','unique-ipsec-request-ids','ipsec-replay-window','netlink-create-echo','ciphertext-only-firewall-admission']
index=['# Individual candidate changes','Each document states the difference, safety rationale, measured evidence and integration limits.']
for section in re.split(r'(?=^## \d+\.)',rationale,flags=re.M)[1:]:
 section=re.split(r'^## (?:Primary source anchors|Applying these proposals)',section,flags=re.M)[0]
 head=section.splitlines()[0];num=int(re.search(r'\d+',head).group());name=f'{num:02d}-{names[num-1]}.md';(changes/name).write_text('#'+section+'\n\nStatus: experimental design; see ../analysis.md for measured scope and integration limits.\n');index.append(f'- [{head.lstrip("# ")}]({name})')
(changes/'README.md').write_text('\n\n'.join(index)+'\n')
print('REPORTS_BUILT',len(allrows),'trial records')
