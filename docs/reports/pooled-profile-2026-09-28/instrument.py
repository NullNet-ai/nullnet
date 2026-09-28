from pathlib import Path
root=Path('/Users/giulianobellini/Desktop/GitHub/nullnet/members/nullnet-client/src/commands')
out=Path('/private/tmp/nullnet-prepared-implementation-20260928/pooled-profile/source')
s=(root/'vxlan.rs').read_text()
helper='''
pub(super) struct Profile {
    operation: &'static str,
    id: u32,
    last: std::time::Instant,
    stages: Vec<(&'static str, u128)>,
}
impl Profile {
    pub(super) fn new(operation: &'static str, id: u32) -> Self {
        Self { operation, id, last: std::time::Instant::now(), stages: Vec::new() }
    }
    pub(super) fn mark(&mut self, stage: &'static str) {
        self.stages.push((stage, self.last.elapsed().as_micros()));
        self.last = std::time::Instant::now();
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        use std::io::Write;
        let line = format!("\\n[nnprofile] {} {} {:?}\\n", self.operation, self.id, self.stages);
        let _ = std::io::stderr().write_all(line.as_bytes());
    }
}
'''
s+='\n'+helper
for fn in ['setup','teardown']:
 a=s.index('pub(crate) async fn '+fn+'(');b=s.index('\nasync fn ',a);part=s[a:b]
 part=part.replace('    let guard = lock(params.vxlan_id).await;', '    let mut prof = Profile::new("'+fn+'_admission", params.vxlan_id);\n    let guard = lock(params.vxlan_id).await;\n    prof.mark("id_lock");',1)
 part=part.replace('    let permit = LIFECYCLE_SLOTS.acquire().await.unwrap();','    let permit = LIFECYCLE_SLOTS.acquire().await.unwrap();\n    prof.mark("lifecycle_slot");\n    drop(prof);',1)
 s=s[:a]+part+s[b:]
a=s.index('async fn reset(');b=s.index('\npub(crate) async fn destroy(',a);part=s[a:b]
part=part.replace('    let e = endpoint.clone();','    let mut total = Profile::new("reset", endpoint.params.vxlan_id);\n    let mut links = Profile::new("reset_links", endpoint.params.vxlan_id);\n    let e = endpoint.clone();',1)
part=part.replace('        let (root, target) = &mut *e.io.lock().unwrap();','        links.mark("blocking_wait");\n        let (root, target) = &mut *e.io.lock().unwrap();\n        links.mark("io_lock");',1)
for old,label in [('        prepared_io::configure(&mut root.route, e.transport, false, None)?;','transport_down'),('        prepared_io::configure(&mut root.route, e.bridge, false, None)?;','bridge_down'),('        prepared_io::configure(&mut root.route, e.forwarding, false, Some(0))?;','forwarding_detach'),('            prepared_io::configure(&mut root.route, index, false, Some(0))?;','outer_detach'),('            prepared_io::configure(&mut target.route, index, false, None)?;','inner_down')]:
 assert old in part;part=part.replace(old,old+'\n        links.mark("'+label+'");',1)
part=part.replace('    if endpoint.params.encrypted {','    total.mark("links_phase");\n    if endpoint.params.encrypted {',1)
part=part.replace('    super::native_netlink::blocking(move || {\n        let (root, target) = &mut *endpoint.io.lock().unwrap();','    total.mark("keys");\n    let mut tail = Profile::new("reset_tail", endpoint.params.vxlan_id);\n    let result = super::native_netlink::blocking(move || {\n        tail.mark("blocking_wait");\n        let (root, target) = &mut *endpoint.io.lock().unwrap();\n        tail.mark("io_lock");',1)
for old,label in [('        root.clear_flows(&[p.ns_net.ip(), p.br_net.ip()])?;','root_conntrack'),('        prepared_io::clear_neighbors(&mut root.route, endpoint.bridge)?;','root_neighbors'),('        prepared_io::clear_addresses(&mut root.route, endpoint.bridge)?;','root_addresses'),('            target.clear_flows(&[p.ns_net.ip()])?;','target_conntrack'),('            prepared_io::clear_neighbors(&mut target.route, index)?;','target_neighbors'),('            prepared_io::clear_addresses(&mut target.route, index)?;','target_addresses'),('            prepared_io::idle(&mut target.route, &format!("{}-in", p.ns_name), index)?;','target_verify')]:
 assert old in part;part=part.replace(old,old+'\n        tail.mark("'+label+'");',1)
part=part.replace('        *endpoint.active.lock().unwrap() = false;','        tail.mark("root_verify");\n        *endpoint.active.lock().unwrap() = false;',1)
part=part.replace('    .handle_err(location!())\n}', '    .handle_err(location!());\n    total.mark("tail_phase");\n    result\n}',1)
s=s[:a]+part+s[b:];(out/'vxlan.rs').write_text(s)
s=(root/'prepared_io.rs').read_text();s=s.replace('        for entry in self.conntrack.dump(257, &[2, 0, 0, 0])? {','        let mut prof = super::vxlan::Profile::new("conntrack", 0);\n        let entries = self.conntrack.dump(257, &[2, 0, 0, 0])?;\n        prof.mark("dump");\n        for entry in entries {',1)
s=s.replace('        Ok(())\n    }\n}\n\nfn flow_deletion', '        prof.mark("filter_delete");\n        Ok(())\n    }\n}\n\nfn flow_deletion',1);(out/'prepared_io.rs').write_text(s)
