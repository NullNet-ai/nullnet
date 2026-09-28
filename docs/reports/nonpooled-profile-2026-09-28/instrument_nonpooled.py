from pathlib import Path
root=Path('/private/tmp/nullnet-prepared-implementation-20260928/on-demand/members/nullnet-client/src/commands')
out=Path('/private/tmp/nullnet-prepared-implementation-20260928/profile-source');out.mkdir(exist_ok=True)
s=(root/'vxlan.rs').read_text()
helper='''
struct Profile {
    operation: &'static str,
    id: u32,
    last: std::time::Instant,
    stages: Vec<(&'static str, u128)>,
}
impl Profile {
    fn new(operation: &'static str, id: u32) -> Self {
        Self { operation, id, last: std::time::Instant::now(), stages: Vec::new() }
    }
    fn mark(&mut self, stage: &'static str) {
        self.stages.push((stage, self.last.elapsed().as_micros()));
        self.last = std::time::Instant::now();
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        eprintln!("[nnprofile] {} {} {:?}", self.operation, self.id, self.stages);
    }
}
'''
s+='\n'+helper
for fn,sem in [('setup','LIFECYCLE_SLOTS'),('teardown','RETIREMENT_SLOTS')]:
 start=s.index('pub(crate) async fn '+fn+'('); end=s.index('\nasync fn ',start)
 part=s[start:end]
 part=part.replace('    let guard = Arc::new(lock(params.vxlan_id).await);','    let mut admission = Profile::new("'+fn+'_admission", params.vxlan_id);\n    let guard = Arc::new(lock(params.vxlan_id).await);\n    admission.mark("id_lock");',1)
 part=part.replace('        .expect("lifecycle admission stays open");','        .expect("lifecycle admission stays open");\n    admission.mark("worker_slot");\n    drop(admission);',1)
 s=s[:start]+part+s[end:]
start=s.index('async fn setup_locked(');end=s.index('\nasync fn create_endpoint_veth',start);part=s[start:end]
part=part.replace('    let handle = &rtnetlink_handle.handle;','    let mut prof = Profile::new("setup", params.vxlan_id);\n    let handle = &rtnetlink_handle.handle;',1)
part=part.replace('    let out_link = if let Some(namespace)', '    prof.mark("namespace");\n    let out_link = if let Some(namespace)',1)
part=part.replace('        configure_ns_in(params, &namespace.handle).await?;', '        prof.mark("veth_create");\n        configure_ns_in(params, &namespace.handle).await?;\n        prof.mark("veth_config");',1)
part=part.replace('    let br_link = LinkUnspec', '    prof.mark("bridge_lease");\n    let br_link = LinkUnspec',1)
part=part.replace('    if params.local_ip == params.remote_ip {','    prof.mark("bridge_config");\n    if params.local_ip == params.remote_ip {',1)
part=part.replace('    if let Some(namespace) = &namespace {\n        namespace.validate()', '    prof.mark("transport");\n    if let Some(namespace) = &namespace {\n        namespace.validate()',1)
part=part.replace('    Ok(namespace)','    prof.mark("validate");\n    Ok(namespace)',1)
s=s[:start]+part+s[end:]
start=s.index('async fn teardown_locked(');end=s.index('// helpers ',start);part=s[start:end]
part=part.replace('    READY','    let mut prof = Profile::new("retire", params.vxlan_id);\n    READY',1)
part=part.replace('    // Parent deletion','    prof.mark("xfrm_remove");\n    // Parent deletion',1)
part=part.replace('    cache.remove_overlay', '    prof.mark("cleanup_worker");\n    cache.remove_overlay',1)
part=part.replace('    Ok(())','    prof.mark("namespace_remove");\n    Ok(())',1)
s=s[:start]+part+s[end:];(out/'vxlan.rs').write_text(s)
s=(root/'vxlan_cleanup.rs').read_text();s=s.replace('    u32,\n    String,','    u32,\n    String,',1)
s=s.replace('    oneshot::Sender<Result<(), String>>,','    oneshot::Sender<Result<(), String>>,\n    std::time::Instant,',1)
s=s.replace('(group, _, _, _, _)','(group, _, _, _, _, _)').replace('(_, _, names, _, _)','(_, _, names, _, _, _)').replace('(_, bridge, _, _, _)','(_, bridge, _, _, _, _)')
s=s.replace('                let result = match remove_groups','                let began = std::time::Instant::now();\n                let waits: Vec<_> = batch.iter().map(|r| r.5.elapsed().as_micros()).collect();\n                let result = match remove_groups',1)
s=s.replace('                    Ok(()) => {','                    Ok(()) => {\n                        eprintln!("[nnprofile] cleanup_delete {} {}", batch.len(), began.elapsed().as_micros());\n                        let reset = std::time::Instant::now();',1)
s=s.replace('                        super::bridge_pool::release(&handle, &bridges).await','                        let result = super::bridge_pool::release(&handle, &bridges).await;\n                        eprintln!("[nnprofile] cleanup_bridge {} {}", batch.len(), reset.elapsed().as_micros());\n                        result',1)
s=s.replace('                for (_, _, _, _guard, response) in batch.drain(..) {','                eprintln!("[nnprofile] cleanup_wait {:?}", waits);\n                for (_, _, _, _guard, response, _) in batch.drain(..) {',1)
s=s.replace('.send((group, bridge, names, guard, response))','.send((group, bridge, names, guard, response, std::time::Instant::now()))',1)
(out/'vxlan_cleanup.rs').write_text(s)
