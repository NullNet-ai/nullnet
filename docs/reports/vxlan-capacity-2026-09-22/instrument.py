from pathlib import Path
p=Path('/root/nullnet-review-20260922/members/nullnet-client/src/commands/vxlan.rs');s=p.read_text();Path('/tmp/nullnet-capacity-vxlan-original.rs').write_text(s)
helper='''
tokio::task_local! { static CAP_WAIT: std::cell::Cell<f64>; }
struct CapacityProfile { id:u32, cross:bool, docker:bool, last:std::time::Instant, start:std::time::Instant, stages:Vec<(&'static str,f64)> }
impl CapacityProfile {
 fn new(p:&VxlanSetupParams)->Self { let now=std::time::Instant::now(); Self{id:p.vxlan_id,cross:p.local_ip!=p.remote_ip,docker:p.docker_container.is_some(),last:now,start:now,stages:Vec::new()} }
 fn mark(&mut self,name:&'static str){let now=std::time::Instant::now();self.stages.push((name,now.duration_since(self.last).as_secs_f64()*1000.0));self.last=now;}
}
impl Drop for CapacityProfile { fn drop(&mut self){ eprintln!("CAPACITY_PROFILE {{\\"id\\":{},\\"cross\\":{},\\"docker\\":{},\\"total_ms\\":{},\\"command_admission_ms\\":{},\\"stages\\":[{}]}}",self.id,self.cross,self.docker,self.start.elapsed().as_secs_f64()*1000.0,CAP_WAIT.try_with(|x|x.get()).unwrap_or(0.0),self.stages.iter().map(|(n,t)|format!("[{:?},{}]",n,t)).collect::<Vec<_>>().join(",")); } }
'''
a=s.index('pub(crate) async fn setup(');s=s[:a]+helper+s[a:];a=s.index('pub(crate) async fn setup(');b=s.index('\nasync fn setup_locked',a);part=s[a:b];part=part.replace('    let _guard = lock(params.vxlan_id).await;','    CAP_WAIT.scope(std::cell::Cell::new(0.0), async {\n    let mut profile=CapacityProfile::new(params);\n    let _guard = lock(params.vxlan_id).await;\n    profile.mark("net_id_lock");');part=part.replace('setup_locked(rtnetlink_handle, params).await','setup_locked(rtnetlink_handle, params, &mut profile).await');part=part.replace('    result\n}', '    result\n    }).await\n}');s=s[:a]+part+s[b:]
a=s.index('async fn setup_locked(');b=s.index('\n/// Same host:',a);part=s[a:b].replace('    params: &VxlanSetupParams,','    params: &VxlanSetupParams,\n    profile: &mut CapacityProfile,',1)
for old,new in [
('    // Create the peer', '    profile.mark("namespace_or_docker_lookup");\n    // Create the peer'),
('    let peer = LinkUnspec', '    profile.mark("veth_stale_lookup_cleanup");\n    let peer = LinkUnspec'),
('    configure_ns_in(params, ns_pid).await?;', '    profile.mark("veth_create");\n    configure_ns_in(params, ns_pid).await?;\n    profile.mark("namespace_address_up_route");'),
('    let br_link = get_link_by_name', '    profile.mark("bridge_lookup_cleanup_create");\n    let br_link = get_link_by_name'),
('    // Attach the root-namespace', '    profile.mark("bridge_address_mtu_up");\n    // Attach the root-namespace'),
('    if params.local_ip == params.remote_ip {','    profile.mark("veth_lookup_attach");\n    if params.local_ip == params.remote_ip {'),
('        setup_same_host(handle, params, br_link.header.index).await?;', '        setup_same_host(handle, params, br_link.header.index).await?;\n        profile.mark("same_host_macsec");'),
('setup_cross_host(handle, params, br_link.header.index).await?', 'setup_cross_host(handle, params, br_link.header.index, profile).await?'),
('    Ok(())','    profile.mark("forwarding_sysctl_iptables");\n    Ok(())')]:
 assert old in part,old;part=part.replace(old,new,1)
s=s[:a]+part+s[b:]
a=s.index('async fn setup_cross_host(');b=s.index('\npub(crate) async fn teardown',a);part=s[a:b].replace('    br_index: u32,','    br_index: u32,\n    profile: &mut CapacityProfile,',1)
part=part.replace('    handle\n        .link()','    profile.mark("cross_host_stale_lookup_cleanup");\n    handle\n        .link()',1).replace('    let vxlan_link =','    profile.mark("vxlan_create");\n    let vxlan_link =',1).replace('    if params.encrypted {','    profile.mark("vxlan_lookup_attach_up");\n    if params.encrypted {',1).replace('    Ok(())','    profile.mark("ipsec_key_states_policies");\n    Ok(())',1);s=s[:a]+part+s[b:]
for name in ['command_status','command_output']:
 a=s.index('async fn '+name+'(');b=s.index('\n}',a)+2;part=s[a:b].replace('    let _permit =','    let cap_start=std::time::Instant::now();\n    let _permit =',1).replace('    command.kill_on_drop', '    let _=CAP_WAIT.try_with(|x|x.set(x.get()+cap_start.elapsed().as_secs_f64()*1000.0));\n    command.kill_on_drop',1);s=s[:a]+part+s[b:]
p.write_text(s)

p=Path('/root/nullnet-review-20260922/members/nullnet-client/src/control_channel.rs');s=p.read_text();Path('/tmp/nullnet-capacity-control-original.rs').write_text(s)
a=s.index('async fn handle_vxlan_setup(');b=s.index('async fn ',a+30);part=s[a:b]
part=part.replace('    let egress_steer =','    let cap_handler=std::time::Instant::now();\n    let egress_steer =',1)
part=part.replace('    let error_code = if setup_result.is_err()', '    let cap_setup_ms=init_t.elapsed().as_secs_f64()*1000.0;\n    let error_code = if setup_result.is_err()',1)
part=part.replace('    Ok(())', '    eprintln!("CAPACITY_CONTROL id={} total_ms={} setup_ms={} post_setup_ms={}",vxlan_id,cap_handler.elapsed().as_secs_f64()*1000.0,cap_setup_ms,init_t.elapsed().as_secs_f64()*1000.0-cap_setup_ms);\n    Ok(())')
p.write_text(s[:a]+part+s[b:])
