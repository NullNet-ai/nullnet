"""Validate final report evidence offline; no network actions."""
import ast,json,pathlib
R=pathlib.Path(__file__).resolve().parent
read=lambda p:json.loads(p.read_text())
selected=0
for host in [103,104]:
 p=R/f'raw-{host}'
 patterns=['current-same_macsec-separate-current-*.json','current-unique_ipsec-separate-current-*.json','standalone-before-current-same_macsec-separate-current-*.json','standalone-before-current-unique_ipsec-separate-current-*.json','dedicated-verified-native-same_macsec-batch-exact-*.json','dedicated-verified-native-marked_ipsec-batch-exact-*.json','dedicated-standalone-verified-native-same_macsec-batch-exact-*.json','dedicated-standalone-verified-native-marked_ipsec-batch-exact-*.json']
 for pattern in patterns:
  ds=[read(f) for f in p.glob(pattern)];assert len(ds)==3,(host,pattern,len(ds))
  for d in ds:
   assert d['n']==1000 and len(d['endpoints'])==1000 and not d['errors']
   assert d['workers']==32 and d['slots']==8
   assert d['remaining']==d['before'] and d['remaining_container_peers']==0
   if pattern.startswith('dedicated'):
    assert d['bridge_pool'] and d['link_echo'] and not d['pooled']
    assert d['pool_clean']=={'idle_bridges':1000,'addresses':0,'neighbors':0,'dynamic_fdb':0,'mdb':0}
    assert d['teardown']['bridge_reset']>0 and d['pool_finalize']>0
    if d['mode']=='marked_ipsec':
     assert d['unique_reqid'] and d['shared_inbound'] and d['replay_window']==4096
     assert d['inventory']['policies']==1001 and d['inventory']['states']==2000
   selected+=1
 ds=read(p/'churn-dedicated.json');assert len(ds)==2
 for d in ds:
  assert len(d['rounds'])==6 and d['container_peers']==0 and d['batch']==256 and d['live_endpoints']==512
  assert all(len(w['endpoints'])==256 and all(e['completion']>=e['seconds'] for e in w['endpoints']) for w in d['rounds'])
 d=read(p/'samehost-dedicated-proof.json')
 assert len(d['pings'])==64 and not any(d['pings']) and d['missing_key']!=0 and d['survivor']==0
 assert len(d['reuse'])==5 and all(not any(x['codes']) for x in d['reuse'])
 assert 'generation exited' in d['stale_generation_rejected'] and not any(d['restart']['codes'])
 assert '3 packets transmitted, 3 received' in d['mtu']
 ds=read(p/'partial-failure-dedicated.json');assert len(ds)==2 and all(d['restored']==d['survivors'] and d['retry_success'] for d in ds)
 d=read(p/'final-audit.json')
 assert all(d[k] for k in ['original_containers_unchanged','original_links_unchanged','routes_unchanged'])
 assert not any(d[k] for k in ['remaining_benchmark_containers','remaining_benchmark_namespaces','remaining_test_peers','remaining_benchmark_processes','running_experiment_units'])
d=read(R/'dedicated-marked-proof.json')
assert d['configuration']=={'port':4790,'replay_window':4096}
assert all(x['success']==32 for x in d['connectivity']) and all(x['received']==x['expected'] for x in d['vlan_isolation'])
assert len(d['edge_reuse'])==3 and all(not any(x['codes']) for x in d['edge_reuse'])
assert len(d['clean_pool_resets'])==6
assert all(ast.literal_eval(x['proof'])=={'idle_bridges':1,'addresses':0,'neighbors':0,'dynamic_fdb':0,'mdb':0} for x in d['clean_pool_resets'])
assert d['wrong_binding_ping']!=0 and d['binding_survivor']==0 and d['wrong_key']!=0 and d['wrong_key_survivor']==0
assert d['missing_crypto']!=0 and d['missing_capture']['udp_vxlan_lines']==0
assert all('3 packets transmitted, 3 received' in d[k] for k in ['mtu','root_to_container'])
counter=lambda text:int(dict(l.split() for l in text.splitlines())['XfrmInStateSeqError'])
assert counter(d['replay_after'])>counter(d['replay_before'])
frames=read(R/'frame-parity.json');assert {d['mode'] for d in frames}=={'current_docker','dedicated_marked'}
for d in frames:
 assert len(d['directions'])==2
 assert all(x['exact_counts']==[3]*8 and not x['foreign_endpoint_frames'] for x in d['directions'])
 assert d['gateway_ping']==0 and d['remote_ping']==0 and d['forward_drop_ping']!=0 and d['routed']['ping']==0
scale=read(R/'scale-dedicated-proof.json');assert len(scale)==1
for d in scale:
 assert d['n']==1000 and all(x['success']==1000 for x in d['connectivity']) and len(d['traffic'])==8
 assert all('error' not in x['result'] for x in d['traffic'])
 parse=lambda rows:[{k:int(v) for k,v in (line.split() for line in text.splitlines())} for text in rows]
 before=parse(d['xfrm_phases']['before_install']);installed=parse(d['xfrm_phases']['after_install']);final=parse(d['xfrm_stats'])
 assert all(not any(x.values()) for x in before)
 assert installed==final==parse(d['xfrm_phases']['after_pings'])
 assert all(parse(t['xfrm_stats'])==installed for t in d['traffic'])
 assert all(not any(v for k,v in x.items() if k!='XfrmInNoStates') for x in final)
records=list(R.glob('traffic-dedicated-repeat-*.json'));assert len(records)==6
for f in records:
 d=read(f);assert all(x['success']==32 for x in d['connectivity'])
 assert len(d['throughput'])==4 and all('error' not in x['result'] for x in d['throughput'])
 assert d['udp_small']['end']['sum_received']['lost_packets']==0
assert selected==48
checks={'selected_1000_endpoint_sequences':selected,'samehost_restart_reuse_hosts':2,'mixed_churn_cases':4,'partial_failure_cases':4,'samehost_macsec_packet_proof':True,'marked_ipsec_packet_proof':True,'seeded_bridge_reset_checks':6,'frame_and_routed_parity_paths':2,'repeated_dataplane_trials':6,'real_1000_edge_topologies':1,'scale_directional_ping_successes':2000,'scale_post_install_xfrm_error_increments':0,'scale_install_missing_sa_packets':[x['XfrmInNoStates'] for x in installed],'clean_lab_audits':2}
(R/'validation-summary.json').write_text(json.dumps(checks,indent=2)+'\n');print(json.dumps(checks))
