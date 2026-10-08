"""Align bridge-free operation columns with the existing C256 report tables."""
TC = ['setup/transport_qdisc', 'setup/outer_qdisc', 'setup/transport_egress_mark',
      'setup/transport_default_drop', 'setup/transport_gateway_pass', 'setup/outer_gateway_pass',
      'setup/authenticated_redirect', 'setup/outer_redirect']
FRESH_CPU = {
    'SETUP · Veth creation': ['setup/create_veth'],
    'VXLAN creation': ['setup/create_vxlan'],
    'TC qdisc + three filters': TC,
    'Two IPsec states + outbound policy': ['setup/install_ipsec_state', 'setup/install_ipsec_policy'],
    'Two addresses': ['setup/add_container_address', 'setup/add_gateway_address'],
    'Two attach + enable requests': [],
    'Bridge + container peer enable': ['setup/enable_container'],
    'Six setup GETLINK lookups/checks': ['setup/create_vxlan_lookup', 'setup/lookup_container_peer',
        'setup/readiness_transport', 'setup/readiness_outer', 'setup/readiness_container'],
    'RESET · Two disable requests': ['teardown/disable_transport'],
    'Remove IPsec states + policy': ['teardown/remove_ipsec_state', 'teardown/remove_ipsec_policy'],
    'Acknowledged batch device deletion': ['teardown/batch_delete_veth_vxlan'],
    'Four scoped conntrack deletions': ['teardown/exact_conntrack_delete'],
    'Remove bridge address': [], 'Bridge GETLINK check': [],
    'Neighbor dump · separate from requests': [],
}
FRESH_RTNL = {
    'SETUP · Create veth pair': ['setup/create_veth'],
    'Look up container peer': ['setup/lookup_container_peer'],
    'Create VXLAN': ['setup/create_vxlan'], 'Look up VXLAN index': ['setup/create_vxlan_lookup'],
    'TC qdisc': ['setup/transport_qdisc', 'setup/outer_qdisc'],
    'Three TC filters': [k for k in TC if 'qdisc' not in k],
    'Two IPsec states + policy': ['setup/install_ipsec_state', 'setup/install_ipsec_policy'],
    'Add container address': ['setup/add_container_address'],
    'Add bridge address': ['setup/add_gateway_address'],
    'Attach outer veth + enable': [], 'Attach VXLAN + enable': [], 'Enable bridge': [],
    'Enable container peer': ['setup/enable_container'],
    'Readiness: VXLAN': ['setup/readiness_transport'], 'Readiness: outer veth': ['setup/readiness_outer'],
    'Readiness: bridge': [], 'Readiness: container peer': ['setup/readiness_container'],
    'RESET · Disable VXLAN': ['teardown/disable_transport'],
    'Remove IPsec states + policy': ['teardown/remove_ipsec_state', 'teardown/remove_ipsec_policy'],
    'Batch-delete VXLAN + veths': ['teardown/batch_delete_veth_vxlan'],
    'Four scoped conntrack deletes': ['teardown/exact_conntrack_delete'],
    'Disable bridge': [], 'Dump bridge neighbors': [], 'Remove bridge address': [], 'Verify bridge GETLINK': [],
}
POOL = {
    'ACTIVATE · Two IPsec states': ['setup/install_ipsec_state'],
    'Outbound IPsec policy': ['setup/install_ipsec_policy'],
    'Add container address': ['setup/add_container_address'],
    'Enable container peer': ['setup/enable_container'], 'Add bridge address': ['setup/add_gateway_address'],
    'Attach outer veth + enable': [], 'Attach VXLAN + enable': [], 'Enable bridge': [],
    'Enable transport again': ['setup/enable_transport_again'],
    'Readiness: VXLAN': ['setup/readiness_transport'], 'Readiness: outer veth': ['setup/readiness_outer'],
    'Readiness: bridge': [], 'Readiness: container peer': ['setup/readiness_container'],
    'RESET · Disable VXLAN': ['teardown/disable_transport'],
    'Disable bridge': [], 'Detach outer veth': [], 'Detach VXLAN': [],
    'Disable container peer': ['teardown/disable_container'],
    'Remove outbound IPsec policy': ['teardown/remove_ipsec_policy'],
    'Remove two IPsec states': ['teardown/remove_ipsec_state'],
    'Four exact conntrack deletes': ['teardown/exact_conntrack_delete'],
    'Dump container neighbors': ['teardown/container_neighbors_dump'], 'Dump bridge neighbors': [],
    'Remove container address': ['teardown/remove_container_address'],
    'Remove bridge address': ['teardown/remove_gateway_address'],
}
EXTRA_SETUP = {
    'Enable outer veth without bridge': ['setup/enable_outer'],
    'Enable VXLAN without bridge': ['setup/enable_transport'],
    'Enable transport again': ['setup/enable_transport_again'],
    'Add two host peer routes': ['setup/add_peer_ip_route', 'setup/add_peer_gateway_route'],
}
EXTRA_RESET = {
    'Disable outer veth without bridge': ['teardown/disable_outer'],
    'Remove two host peer routes': ['teardown/remove_peer_ip_route', 'teardown/remove_peer_gateway_route'],
    'Dump root veth / VXLAN neighbors': ['teardown/root_outer_neighbors_dump', 'teardown/root_transport_neighbors_dump'],
}
RENAMES = {'TC qdisc + three filters': 'TC qdisc(s) + filters', 'Six setup GETLINK lookups/checks': 'Setup GETLINK lookups/checks',
           'TC qdisc': 'TC qdisc(s)', 'Three TC filters': 'TC filters',
           'Add bridge address': 'Add host gateway address', 'Remove bridge address': 'Remove host gateway address'}


def dataset(data, host, lifecycle, kind):
    return data['hosts'][host]['conditions'][lifecycle+'-redirect']['concurrency']['256'][kind]


def extend(rows, data, lifecycle, kind, mapping):
    metric = 'cpu_ms_per_cycle' if kind == 'cpu' else 'hold_ms_per_cycle'
    def values(keys):
        return [sum(dataset(data, h, lifecycle, kind)['rows'].get(k, {}).get(metric, 0) for k in keys) for h in ['103', '104']]
    setup_extra = {k:v for k,v in EXTRA_SETUP.items() if k not in mapping}
    reset_extra = EXTRA_RESET if lifecycle == 'pooled' else {}
    used = [key for keys in [*mapping.values(), *setup_extra.values(), *reset_extra.values()] for key in keys]
    assert len(set(used)) == len(used), used
    for h in ['103', '104']:
        source = dataset(data, h, lifecycle, kind)
        assert set(source['rows'])-{'other_actor'} == set(used), (h, lifecycle, kind, set(source['rows'])-set(used))
    result = []
    inserted_setup, inserted_reset = False, False
    for row in rows:
        name = row[0]
        if name.startswith('RESET') and not inserted_setup:
            result.extend([[k, '0.0000', '0.0000']+[f'{v:.4f}' for v in values(keys)] for k,keys in setup_extra.items()])
            inserted_setup = True
        if ('subtotal' in name.lower() or name.startswith('Total')) and not inserted_reset:
            result.extend([[k, '0.0000', '0.0000']+[f'{v:.4f}' for v in values(keys)] for k,keys in reset_extra.items()])
            inserted_reset = True
        if name in mapping:
            extra = values(mapping[name])
        elif name.startswith('Setup subtotal') or name.startswith('Activation subtotal'):
            extra = values([k for k in used if k.startswith('setup/')])
        elif name.startswith('Teardown subtotal') or name.startswith('Reset subtotal'):
            extra = values([k for k in used if k.startswith('teardown/')])
        elif name.startswith('Synchronous request subtotal') or name.startswith('Total'):
            extra = values(used)
        else:
            raise ValueError(name)
        result.append([RENAMES.get(name, name), *row[1:], *[f'{v:.4f}' for v in extra]])
    return result


def family(key):
    if 'ipsec' in key: return 'IPsec install/remove'
    if 'conntrack' in key: return 'Conntrack deletes'
    if 'neighbor' in key: return 'Neighbor dump'
    if key.endswith('_route'): return 'Peer routes'
    if 'lookup' in key or 'readiness' in key or 'idle_' in key: return 'GETLINK queries'
    if 'create_' in key: return 'Device creation'
    if any(x in key for x in ['qdisc', 'gateway_pass', 'redirect', 'egress_mark', 'default_drop', 'authenticated_accept']): return 'TC installation'
    if 'address' in key: return 'Addresses'
    if 'batch_delete' in key: return 'Batch deletion'
    if 'detach' in key: return 'Detach ports'
    if 'disable' in key: return 'Disable links'
    if 'attach' in key or 'enable' in key: return 'Attach / enable'
    raise ValueError(key)
