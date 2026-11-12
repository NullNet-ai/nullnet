#![allow(non_snake_case)]

use crate::graphviz::render_graphviz;
use crate::nullnet_grpc_impl::{NullnetGrpcImpl, build_service_triggers};
use crate::services::clients::Client;
use crate::services::firewall::FilterPolicy;
use crate::services::input::{ServicesToml, StackMap, apply_config_update};
use crate::services::service_info::ServiceInfo;
use crate::timeout::{apply_timeouts, reap_idle_backend_edges};
use nullnet_grpc_lib::nullnet_grpc::{NetMessage, ServiceProtocol, net_message};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};

/// The single stack name used by all fixture-based tests. Fixtures pre-date
/// the per-stack split, so they're loaded into one virtual stack.
const TEST_STACK: &str = "default";

fn into_stack_map(inner: HashMap<String, ServiceInfo>) -> StackMap {
    HashMap::from([(TEST_STACK.to_string(), inner)])
}

fn stack_view(guard: &StackMap) -> &HashMap<String, ServiceInfo> {
    guard.get(TEST_STACK).expect("default stack missing")
}

fn stack_view_mut(guard: &mut StackMap) -> &mut HashMap<String, ServiceInfo> {
    guard.get_mut(TEST_STACK).expect("default stack missing")
}

fn ip(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a, b, c, d))
}

async fn assert_net_ids_in_use(server: &NullnetGrpcImpl, expected: u32) {
    // A net id is returned to the pool only once both endpoints confirm the
    // teardown, so let any in-flight teardown finish before sampling.
    server.orchestrator().settle_teardowns().await;
    let in_use = server.orchestrator().net_ids_in_use().await;
    assert_eq!(
        in_use, expected,
        "expected {expected} NET IDs in use, got {in_use}"
    );
}

/// Strip non-deterministic parts (NET IDs, timing) from graphviz edge labels
/// so that structural comparison is possible.
fn normalize_graphviz(graphviz: &str) -> String {
    graphviz
        .lines()
        .map(|line| {
            if line.contains("->") {
                if let Some(bracket_pos) = line.find(" [label=") {
                    let mut stripped = line[..bracket_pos].to_string();
                    stripped.push(';');
                    stripped
                } else {
                    line.to_string()
                }
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn fixture_path(fixture: &str, file: &str) -> String {
    format!(
        "{}/tests/fixtures/{fixture}/{file}",
        env!("CARGO_MANIFEST_DIR")
    )
}

async fn load_fixture(fixture: &str) -> StackMap {
    load_config(fixture, "services.toml").await
}

async fn load_config(fixture: &str, file: &str) -> StackMap {
    let inner = ServicesToml::load_file(&fixture_path(fixture, file))
        .await
        .expect("failed to load test config");
    into_stack_map(inner)
}

async fn register_services(server: &NullnetGrpcImpl, ip_map: &HashMap<&str, IpAddr>, port: u16) {
    let mut services = server.services().write().await;
    let stack = services
        .get_mut(TEST_STACK)
        .expect("default stack missing in test setup");
    for (&name, &svc_ip) in ip_map {
        if let Some(si) = stack.get_mut(name) {
            si.add_replica(svc_ip, port, None);
        }
    }
    drop(services);

    let unique_ips: HashSet<_> = ip_map.values().collect();
    for &svc_ip in unique_ips {
        server.orchestrator().register_fake_client(svc_ip).await;
    }
}

fn assert_graphviz(services: &StackMap, fixture: &str, expected_file: &str) {
    let inner = services.get(TEST_STACK).expect("default stack missing");
    let actual = render_graphviz(inner);
    let expected_path = fixture_path(fixture, expected_file);

    println!("--- {expected_file} ---\n{actual}");

    if std::env::var_os("UPDATE_GRAPHVIZ").is_some() {
        std::fs::write(&expected_path, &actual).expect("failed to update expected dot file");
        return;
    }

    let expected = std::fs::read_to_string(&expected_path).unwrap_or_else(|_| {
        std::fs::write(&expected_path, &actual).expect("failed to write expected dot file");
        println!("BOOTSTRAPPED: wrote {expected_path}");
        actual.clone()
    });

    assert_eq!(
        normalize_graphviz(&actual),
        normalize_graphviz(&expected),
        "Graphviz mismatch for {expected_file}"
    );
}

fn client_entry_holds(guard: &StackMap, service: &str, client: &Client) -> Option<usize> {
    match stack_view(guard).get(service) {
        Some(ServiceInfo::Registered(reg)) => reg.replicas().iter().find_map(|replica| {
            replica
                .clients()
                .get(client)
                .map(crate::services::clients::ClientInfo::active_holds)
        }),
        _ => None,
    }
}

fn client_entry_net_id(guard: &StackMap, service: &str, client: &Client) -> Option<u32> {
    match stack_view(guard).get(service) {
        Some(ServiceInfo::Registered(reg)) => reg.replicas().iter().find_map(|replica| {
            replica
                .clients()
                .get(client)
                .map(crate::services::clients::ClientInfo::net_id)
        }),
        _ => None,
    }
}

fn setup_net_ids(log: &[NetMessage]) -> Vec<u32> {
    log.iter()
        .filter_map(|message| match &message.message {
            Some(net_message::Message::VxlanSetup(setup)) => Some(setup.vxlan_id),
            Some(net_message::Message::VlanSetup(setup)) => Some(setup.vlan_id),
            _ => None,
        })
        .collect()
}

fn teardown_net_ids(log: &[NetMessage]) -> Vec<u32> {
    log.iter()
        .filter_map(|message| match &message.message {
            Some(net_message::Message::VxlanTeardown(teardown)) => Some(teardown.vxlan_id),
            Some(net_message::Message::VlanTeardown(teardown)) => Some(teardown.vlan_id),
            _ => None,
        })
        .collect()
}

/// A *completed* request: the proxy resolves an upstream, then the connection
/// closes. Both halves matter — the server pins a client while it has open
/// connections, so a helper that only ever opened would leave every client
/// unreapable and quietly disable the timeout tests below.
async fn setup_proxy_edge(
    server: &NullnetGrpcImpl,
    service_name: &str,
    proxy_ip: IpAddr,
    client_ip: &str,
) {
    server
        .handle_proxy_request(service_name, proxy_ip, client_ip)
        .await
        .expect("proxy request failed");
    server
        .mark_connection_closed(service_name, proxy_ip, client_ip)
        .await;
}

/// Trigger the backend edge at `port` from `initiator_ip` (acting as the
/// announcing host). Mirrors the gRPC `BackendTrigger` entry point.
async fn trigger_backend_edge(
    server: &NullnetGrpcImpl,
    initiator_name: &str,
    initiator_ip: IpAddr,
    port: u16,
) {
    server
        .handle_backend_trigger(initiator_name, port, initiator_ip, None)
        .await
        .expect("backend trigger failed");
}

/// Set up a backend edge initiated by a specific replica (ip + optional
/// docker container). Used when co-located replicas need independent edges.
async fn setup_backend_edge_for_replica(
    server: &NullnetGrpcImpl,
    initiator_name: &str,
    initiator_ip: IpAddr,
    initiator_docker: Option<&str>,
    port: u16,
) {
    server
        .setup_backend_edge(
            TEST_STACK,
            initiator_name,
            initiator_ip,
            initiator_docker,
            port,
        )
        .await
        .expect("setup_backend_edge failed");
}

// ===========================================================================
// proxy_timeout: proxy1→A+B, proxy2→A.
// A has timeout=1, B has timeout=5 (B kept well above A so the "B survives A's
// timeout" assertions have a wide margin under real-wall-clock CI contention).
// ===========================================================================

const PROXY_TIMEOUT: &str = "proxy_timeout";

pub(crate) async fn proxy_timeout_setup() -> NullnetGrpcImpl {
    let services = load_fixture(PROXY_TIMEOUT).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    let ip_map = HashMap::from([("A", ip(1, 1, 1, 1)), ("B", ip(2, 2, 2, 2))]);
    let proxy1 = ip(5, 5, 5, 5);
    let proxy2 = ip(6, 6, 6, 6);
    register_services(&server, &ip_map, 8080).await;
    server.orchestrator().register_fake_client(proxy1).await;
    server.orchestrator().register_fake_client(proxy2).await;

    setup_proxy_edge(&server, "A", proxy1, "10.0.0.1").await;
    setup_proxy_edge(&server, "B", proxy1, "10.0.0.1").await;
    setup_proxy_edge(&server, "A", proxy2, "10.0.0.2").await;

    assert_net_ids_in_use(&server, 3).await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, PROXY_TIMEOUT, "start.dot");
    drop(guard);

    server
}

#[tokio::test]
async fn mass_expiry_allows_reads_between_client_teardowns() {
    let server = proxy_timeout_setup().await;
    let proxy = ip(5, 5, 5, 5);
    for i in 3..131 {
        setup_proxy_edge(&server, "A", proxy, &format!("10.0.0.{i}")).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let (log, gate) = server.orchestrator().register_gated_client(proxy).await;
    let worker = server.clone();
    let reap = tokio::spawn(async move {
        crate::timeout::reap_ingress_timeouts(worker.services(), worker.orchestrator()).await;
    });
    wait_for_log(&log, |messages| !messages.is_empty()).await;
    let reader = server.clone();
    let read = tokio::spawn(async move {
        let _guard = reader.services().read().await;
    });
    tokio::task::yield_now().await;
    // Unblock one send if the bounded control queue is already full.
    gate.add_permits(1);
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), read).await;
    gate.add_permits(1000);
    reap.await.unwrap();
    assert!(result.is_ok(), "bulk expiry monopolized the services lock");
    let guard = server.services().read().await;
    let ServiceInfo::Registered(reg) = &stack_view(&guard)["A"] else {
        unreachable!()
    };
    assert_eq!(reg.client_count(), 0);
}

#[tokio::test]
async fn concurrent_requests_for_one_client_tear_down_cleanly() {
    let server = std::sync::Arc::new(NullnetGrpcImpl::new_for_test(
        load_fixture(PROXY_TIMEOUT).await,
    ));
    let service_ip = ip(1, 1, 1, 1);
    let proxy = ip(5, 5, 5, 5);
    register_services(&server, &HashMap::from([("A", service_ip)]), 8080).await;
    server.orchestrator().register_fake_client(proxy).await;

    let mut requests = tokio::task::JoinSet::new();
    for _ in 0..6 {
        let server = server.clone();
        requests.spawn(async move {
            server
                .handle_proxy_request("A", proxy, "10.0.0.1")
                .await
                .expect("proxy request failed");
            server.mark_connection_closed("A", proxy, "10.0.0.1").await;
        });
    }
    while let Some(result) = requests.join_next().await {
        result.unwrap();
    }

    assert_net_ids_in_use(&server, 1).await;
    let client = Client::new("10.0.0.1".into(), Some(proxy));
    assert_eq!(
        client_entry_holds(&*server.services().read().await, "A", &client),
        Some(1),
        "racing requests acquired duplicate edge holds"
    );

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let mut guard = server.services().write().await;
    apply_timeouts(
        stack_view_mut(&mut guard),
        server.orchestrator(),
        TEST_STACK,
    )
    .await;
    drop(guard);
    assert_net_ids_in_use(&server, 0).await;
}

/// After A's timeout (1s), both proxy clients on A expire. B's proxy client
/// survives (timeout=5).
#[tokio::test]
async fn proxy_timeout_A() {
    let server = proxy_timeout_setup().await;

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    let mut guard = server.services().write().await;
    apply_timeouts(stack_view_mut(&mut guard), server.orchestrator(), "default").await;
    assert_graphviz(&guard, PROXY_TIMEOUT, "after_timeout_A.dot");

    // A is still registered but has no proxy clients
    assert!(matches!(
        stack_view(&guard)["A"],
        ServiceInfo::Registered(_)
    ));
    // B's proxy client is still alive
    assert!(matches!(
        stack_view(&guard)["B"],
        ServiceInfo::Registered(_)
    ));
    if let ServiceInfo::Registered(reg) = &stack_view(&guard)["B"] {
        assert_eq!(reg.client_count(), 1);
    }
    if let ServiceInfo::Registered(reg) = &stack_view(&guard)["A"] {
        assert!(!reg.has_clients());
    }
}

/// The point of the open-connection count: a client whose connection is still
/// open is never reaped, however long it idles. A's timeout is 1s; this holds
/// the connection open across many multiples of it and expects A intact.
///
/// This is the regression that motivated the whole rework — before it, a long
/// download or a live WebSocket was torn down mid-transfer with no self-heal.
#[tokio::test]
async fn open_connection_pins_client_past_timeout() {
    let server = proxy_timeout_setup().await;
    let proxy1 = ip(5, 5, 5, 5);

    // A fresh request that does NOT close: one connection still open.
    server
        .handle_proxy_request("A", proxy1, "10.0.0.1")
        .await
        .expect("proxy request failed");

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    {
        let mut guard = server.services().write().await;
        apply_timeouts(stack_view_mut(&mut guard), server.orchestrator(), "default").await;
        if let ServiceInfo::Registered(reg) = &stack_view(&guard)["A"] {
            assert!(
                reg.has_clients(),
                "a client with an open connection must not be reaped on idle"
            );
        }
    }

    // Close it, and the same idle period now reaps: the timeout is a grace
    // window measured from the last close, not a hard cap on the session.
    server.mark_connection_closed("A", proxy1, "10.0.0.1").await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let mut guard = server.services().write().await;
    apply_timeouts(stack_view_mut(&mut guard), server.orchestrator(), "default").await;
    if let ServiceInfo::Registered(reg) = &stack_view(&guard)["A"] {
        assert!(
            !reg.has_clients(),
            "once the last connection closed, the grace window must expire normally"
        );
    }
}

/// A close that arrives without a matching open must not underflow the count.
/// The proxy retries closes, so a duplicate is entirely possible; underflowing
/// to `usize::MAX` would pin the edge forever.
#[tokio::test]
async fn unmatched_close_does_not_underflow() {
    let server = proxy_timeout_setup().await;
    let proxy1 = ip(5, 5, 5, 5);

    // setup_proxy_edge already closed once; close twice more.
    server.mark_connection_closed("A", proxy1, "10.0.0.1").await;
    server.mark_connection_closed("A", proxy1, "10.0.0.1").await;

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let mut guard = server.services().write().await;
    apply_timeouts(stack_view_mut(&mut guard), server.orchestrator(), "default").await;
    if let ServiceInfo::Registered(reg) = &stack_view(&guard)["A"] {
        assert!(
            !reg.has_clients(),
            "a saturating close must leave the client reapable, not pinned"
        );
    }
}

/// After B's timeout (5s), B's proxy client also expires.
/// All ingress connections are gone; all services remain registered.
#[tokio::test]
async fn proxy_timeout_A_then_B() {
    let server = proxy_timeout_setup().await;

    // A expires after 1s
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let mut guard = server.services().write().await;
    apply_timeouts(stack_view_mut(&mut guard), server.orchestrator(), "default").await;
    assert_graphviz(&guard, PROXY_TIMEOUT, "after_timeout_A.dot");
    drop(guard);

    // B expires after 5s total (real wall-clock; wide margin so a loaded CI
    // runner overrunning the earlier sleeps still can't expire B prematurely)
    tokio::time::sleep(std::time::Duration::from_millis(4200)).await;
    let mut guard = server.services().write().await;
    apply_timeouts(stack_view_mut(&mut guard), server.orchestrator(), "default").await;
    assert_graphviz(&guard, PROXY_TIMEOUT, "after_timeout_A_then_B.dot");

    // all services still registered, but no proxy clients left
    for si in stack_view(&guard).values() {
        if let ServiceInfo::Registered(reg) = si {
            assert!(
                !reg.has_clients(),
                "expected no proxy clients after both timeouts"
            );
        }
    }
    drop(guard);

    // All ingress IDs are freed.
    assert_net_ids_in_use(&server, 0).await;
}

/// After 5s+ both A and B expire simultaneously in a single apply.
/// All ingress NET IDs are freed.
#[tokio::test]
async fn proxy_timeout_all_at_once() {
    let server = proxy_timeout_setup().await;

    tokio::time::sleep(std::time::Duration::from_millis(5200)).await;

    let mut guard = server.services().write().await;
    apply_timeouts(stack_view_mut(&mut guard), server.orchestrator(), "default").await;
    assert_graphviz(&guard, PROXY_TIMEOUT, "after_timeout_all.dot");

    for si in stack_view(&guard).values() {
        if let ServiceInfo::Registered(reg) = si {
            assert!(
                !reg.has_clients(),
                "expected no proxy clients after full timeout"
            );
        }
    }
    drop(guard);

    assert_net_ids_in_use(&server, 0).await;
}

/// Config update tightens B's timeout from 5→1. After 1.5s, B's clients
/// are past the new limit and get expired by the config change.
/// A's timeout is unchanged in the config, so the config path doesn't
/// touch A's clients (even though they're past A's own timeout).
#[tokio::test]
async fn proxy_timeout_config_tighten_B() {
    let server = proxy_timeout_setup().await;

    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    let new_config = load_config(PROXY_TIMEOUT, "tighten_B.toml").await;
    let mut guard = server.services().write().await;
    apply_config_update(&mut guard, new_config, server.orchestrator()).await;
    assert_graphviz(&guard, PROXY_TIMEOUT, "after_config_tighten_B.dot");

    // B's proxy client expired due to config tightening
    if let ServiceInfo::Registered(reg) = &stack_view(&guard)["B"] {
        assert!(!reg.has_clients());
    }
    // A's clients are still present (config path only handles config changes)
    if let ServiceInfo::Registered(reg) = &stack_view(&guard)["A"] {
        assert_eq!(reg.client_count(), 2);
    }
}

/// Config update removes A's timeout (1→0). Even after 1.5s, A's clients
/// are NOT expired because the timeout was removed, not tightened.
/// B's timeout is unchanged, so B's client also stays.
#[tokio::test]
async fn proxy_timeout_config_remove_timeout_A() {
    let server = proxy_timeout_setup().await;

    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    let new_config = load_config(PROXY_TIMEOUT, "remove_timeout_A.toml").await;
    let mut guard = server.services().write().await;
    apply_config_update(&mut guard, new_config, server.orchestrator()).await;
    assert_graphviz(&guard, PROXY_TIMEOUT, "after_config_remove_timeout_A.dot");

    // A's timeout was removed — no expiry, clients still present
    assert_eq!(stack_view(&guard)["A"].timeout(), Some(0));
    if let ServiceInfo::Registered(reg) = &stack_view(&guard)["A"] {
        assert_eq!(reg.client_count(), 2);
    }
    // B unchanged
    if let ServiceInfo::Registered(reg) = &stack_view(&guard)["B"] {
        assert_eq!(reg.client_count(), 1);
    }
}

// ===========================================================================
// max_networks: A has max_networks=1 and timeout=1.
// Standard ingress groups seed carriers; overflow balances across them.
// ===========================================================================

const MAX_NETWORKS: &str = "max_networks";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn max_networks_balances_groups_across_standard_carriers() {
    let config: ServicesToml = toml::from_str(
        r#"[[services]]
name = "A"
timeout = 60
max_networks = 10
"#,
    )
    .unwrap();
    let server = std::sync::Arc::new(NullnetGrpcImpl::new_for_test(into_stack_map(
        config.services_map().unwrap(),
    )));
    let proxy = ip(5, 5, 5, 5);
    register_services(&server, &HashMap::from([("A", ip(1, 1, 1, 1))]), 8080).await;
    let proxy_log = server.orchestrator().register_recording_client(proxy).await;
    for i in 1..=10 {
        setup_proxy_edge(&server, "A", proxy, &format!("10.0.0.{i}")).await;
    }
    assert_net_ids_in_use(&server, 10).await;

    let mut requests = tokio::task::JoinSet::new();
    for i in 11..=200 {
        let server = server.clone();
        requests.spawn(async move {
            setup_proxy_edge(&server, "A", proxy, &format!("10.0.0.{i}")).await;
        });
    }
    while let Some(result) = requests.join_next().await {
        result.unwrap();
    }
    assert_net_ids_in_use(&server, 200).await;
    let guard = server.services().read().await;
    let ServiceInfo::Registered(reg) = &stack_view(&guard)["A"] else {
        panic!("A should be registered");
    };
    let mut users = HashMap::<u32, usize>::new();
    for replica in reg.replicas() {
        for ci in replica.clients().values() {
            *users.entry(ci.net_id()).or_default() += 1;
        }
    }
    assert_eq!(users.len(), 200);
    assert!(users.values().all(|count| *count == 1), "{users:?}");
    let policy_clients = reg
        .replicas()
        .iter()
        .flat_map(|replica| replica.clients().values())
        .filter(|info| info.policy())
        .count();
    assert_eq!(policy_clients, 190);
    assert_eq!(reg.dedicated_proxy_networks_count(), 10);
    drop(guard);

    let messages = proxy_log.lock().await;
    let groups: Vec<_> = messages
        .iter()
        .filter_map(|message| match &message.message {
            Some(net_message::Message::VxlanSetup(setup)) => setup.group_transport.as_ref(),
            _ => None,
        })
        .collect();
    assert_eq!(groups.len(), 200);
    assert_eq!(
        groups
            .iter()
            .map(|group| group.id)
            .collect::<HashSet<_>>()
            .len(),
        200
    );
    assert_eq!(
        groups
            .iter()
            .map(|group| group.encryption_key.clone())
            .collect::<HashSet<_>>()
            .len(),
        200
    );
    assert_eq!(
        groups
            .iter()
            .map(|group| group.generation)
            .collect::<HashSet<_>>()
            .len(),
        200
    );
    let mut carrier_users = HashMap::new();
    for group in &groups {
        *carrier_users.entry(group.carrier_id).or_insert(0usize) += 1;
    }
    assert_eq!(carrier_users.len(), 10);
    assert!(carrier_users.values().all(|users| *users == 20));
    drop(messages);
}

#[tokio::test]
async fn max_networks_reuses_one_overflow_group_for_the_same_client_ip() {
    let services = load_fixture(MAX_NETWORKS).await;
    let server = NullnetGrpcImpl::new_for_test(services);
    let proxy = ip(5, 5, 5, 5);
    register_services(&server, &HashMap::from([("A", ip(1, 1, 1, 1))]), 8080).await;
    let proxy_log = server.orchestrator().register_recording_client(proxy).await;

    setup_proxy_edge(&server, "A", proxy, "10.0.0.1").await;
    setup_proxy_edge(&server, "A", proxy, "10.0.0.2").await;
    setup_proxy_edge(&server, "A", proxy, "10.0.0.2").await;

    assert_net_ids_in_use(&server, 2).await;
    let guard = server.services().read().await;
    let ServiceInfo::Registered(reg) = &stack_view(&guard)["A"] else {
        panic!("A should be registered");
    };
    assert_eq!(reg.client_count(), 2);
    assert_eq!(
        reg.replicas()
            .iter()
            .flat_map(|replica| replica.clients().values())
            .filter(|info| info.policy())
            .count(),
        1
    );
    drop(guard);

    let messages = proxy_log.lock().await;
    assert_eq!(
        messages
            .iter()
            .filter(|message| matches!(
                &message.message,
                Some(net_message::Message::VxlanSetup(setup))
                    if setup.group_transport.is_some()
            ))
            .count(),
        2
    );
    let groups: Vec<_> = messages
        .iter()
        .filter_map(|message| match &message.message {
            Some(net_message::Message::VxlanSetup(setup)) => setup.group_transport.as_ref(),
            _ => None,
        })
        .collect();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].carrier_id, groups[1].carrier_id);
    assert_ne!(groups[0].encryption_key, groups[1].encryption_key);
}

/// Full lifecycle:
///   1. First client creates one ingress network
///   2. Second client gets an overflow group
///   3. First client times out while the policy edge survives
///   4. Second client times out (all edges are torn down)
#[tokio::test]
async fn max_networks_reuse_lifecycle() {
    let services = load_fixture(MAX_NETWORKS).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    let ip_map = HashMap::from([("A", ip(1, 1, 1, 1))]);
    let proxy1 = ip(5, 5, 5, 5);
    register_services(&server, &ip_map, 8080).await;
    server.orchestrator().register_fake_client(proxy1).await;

    // 1. First proxy client — creates fresh networks
    setup_proxy_edge(&server, "A", proxy1, "10.0.0.1").await;
    assert_net_ids_in_use(&server, 1).await;

    {
        let guard = server.services().read().await;
        assert_graphviz(&guard, MAX_NETWORKS, "after_first_client.dot");
        let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] else {
            panic!("A should be registered");
        };
        assert_eq!(reg_a.client_count(), 1, "A should have 1 proxy client");
    }

    // 2. Second proxy client on the same proxy — policy overflow.
    //    Separate the two clients by 2s (>> the 1s timeout) so at step 3 C1 is
    //    comfortably expired while C2 is comfortably alive. Timeouts use real
    //    `Instant` (not tokio's pausable clock), so wide margins are needed to
    //    survive wall-clock jitter on loaded CI runners.
    tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
    setup_proxy_edge(&server, "A", proxy1, "10.0.0.2").await;
    assert_net_ids_in_use(&server, 2).await;

    {
        let guard = server.services().read().await;
        assert_graphviz(&guard, MAX_NETWORKS, "after_reuse.dot");
        let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] else {
            panic!("A should be registered");
        };
        assert_eq!(reg_a.client_count(), 2, "A should have 2 proxy clients");
        // Every overflow connection is its own group and net id.
        let net_ids: HashSet<u32> = reg_a
            .replicas()
            .iter()
            .flat_map(|r| r.clients().values().map(|ci| ci.net_id()))
            .collect();
        assert_eq!(net_ids.len(), 2, "overflow clients must not share a group");
    }

    // 3. First client times out — network should stay up.
    //    C1 created at t=0, C2 at t≈2s. Sleep 200ms → apply at t≈2.2s.
    //    C1 aged ≈2.2s (> 1s → expired, ~1.2s margin); C2 aged ≈0.2s
    //    (< 1s → alive, ~0.8s margin). Both margins absorb CI jitter.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    {
        let mut guard = server.services().write().await;
        apply_timeouts(stack_view_mut(&mut guard), server.orchestrator(), "default").await;
        assert_graphviz(&guard, MAX_NETWORKS, "after_first_timeout.dot");

        let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] else {
            panic!("A should be registered");
        };
        assert_eq!(
            reg_a.client_count(),
            1,
            "A should have 1 proxy client after first timeout"
        );
    }
    // The policy ingress edge remains.
    assert_net_ids_in_use(&server, 1).await;

    // 4. Second client times out — network should be torn down.
    //    C2 is aged ≈0.2s here; sleep 1.2s → ≈1.4s total (> 1s → expired).
    //    This is an expiry check, so CI overrun only ages C2 further.
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

    {
        let mut guard = server.services().write().await;
        apply_timeouts(stack_view_mut(&mut guard), server.orchestrator(), "default").await;
        assert_graphviz(&guard, MAX_NETWORKS, "after_second_timeout.dot");

        let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] else {
            panic!("A should be registered");
        };
        assert!(
            !reg_a.has_clients(),
            "A should have no clients after both timeouts"
        );
    }
    // All networks torn down
    assert_net_ids_in_use(&server, 0).await;
}

/// Proxy disconnect tears down both the dedicated and policy ingress edges.
#[tokio::test]
async fn max_networks_proxy_disconnect() {
    let services = load_fixture(MAX_NETWORKS).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    let ip_map = HashMap::from([("A", ip(1, 1, 1, 1))]);
    let proxy1 = ip(5, 5, 5, 5);
    register_services(&server, &ip_map, 8080).await;
    server.orchestrator().register_fake_client(proxy1).await;

    setup_proxy_edge(&server, "A", proxy1, "10.0.0.1").await;
    setup_proxy_edge(&server, "A", proxy1, "10.0.0.2").await;
    assert_net_ids_in_use(&server, 2).await;

    {
        let guard = server.services().read().await;
        assert_graphviz(&guard, MAX_NETWORKS, "before_proxy_disconnect.dot");
        let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] else {
            panic!("A should be registered");
        };
        assert_eq!(reg_a.client_count(), 2);
    }

    // Proxy disconnects — both clients torn down simultaneously
    server
        .orchestrator()
        .handle_node_disconnect(proxy1, server.services())
        .await;

    {
        let guard = server.services().read().await;
        assert_graphviz(&guard, MAX_NETWORKS, "after_proxy_disconnect.dot");
        let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] else {
            panic!("A should be registered");
        };
        assert!(
            !reg_a.has_clients(),
            "A should have no clients after proxy disconnect"
        );
    }
    assert_net_ids_in_use(&server, 0).await;
}

/// When max_networks is reached on one proxy, a different proxy with no
/// existing network falls through to a new ingress edge (soft limit).
#[tokio::test]
async fn max_networks_different_proxy_bypasses() {
    let services = load_fixture(MAX_NETWORKS).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    let ip_map = HashMap::from([("A", ip(1, 1, 1, 1))]);
    let proxy1 = ip(5, 5, 5, 5);
    let proxy2 = ip(6, 6, 6, 6);
    register_services(&server, &ip_map, 8080).await;
    server.orchestrator().register_fake_client(proxy1).await;
    server.orchestrator().register_fake_client(proxy2).await;

    // First client on proxy1 — creates network
    setup_proxy_edge(&server, "A", proxy1, "10.0.0.1").await;
    assert_net_ids_in_use(&server, 1).await;

    {
        let guard = server.services().read().await;
        assert_graphviz(&guard, MAX_NETWORKS, "after_proxy1_client.dot");
    }

    // proxy2 connects — max_networks=1 is reached, but proxy2 has no existing
    // network to reuse, so it falls through and creates a new one
    setup_proxy_edge(&server, "A", proxy2, "10.0.0.2").await;
    assert_net_ids_in_use(&server, 2).await;

    {
        let guard = server.services().read().await;
        assert_graphviz(&guard, MAX_NETWORKS, "after_proxy2_bypasses.dot");
        let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] else {
            panic!("A should be registered");
        };
        assert_eq!(
            reg_a.client_count(),
            2,
            "A should have 2 proxy clients (different proxies)"
        );
        // Both clients should have DIFFERENT net_ids
        let net_ids: HashSet<u32> = reg_a
            .replicas()
            .iter()
            .flat_map(|r| r.clients().values().map(|ci| ci.net_id()))
            .collect();
        assert_eq!(
            net_ids.len(),
            2,
            "clients on different proxies should have different net_ids"
        );
    }
}

// ===========================================================================
// triggers_changed: A routes backend port 5555 to C; D routes port 5555 to C.
// The topology contains proxy1→A, A→C, and D→C.
// ===========================================================================

const TRIGGERS_CHANGED: &str = "triggers_changed";

async fn triggers_changed_setup() -> NullnetGrpcImpl {
    let services = load_fixture(TRIGGERS_CHANGED).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    let ip_map = HashMap::from([
        ("A", ip(1, 1, 1, 1)),
        ("C", ip(3, 3, 3, 3)),
        ("D", ip(4, 4, 4, 4)),
    ]);
    let proxy1 = ip(5, 5, 5, 5);
    register_services(&server, &ip_map, 8080).await;
    server.orchestrator().register_fake_client(proxy1).await;

    setup_proxy_edge(&server, "A", proxy1, "10.0.0.1").await;
    trigger_backend_edge(&server, "A", ip(1, 1, 1, 1), 5555).await;
    trigger_backend_edge(&server, "D", ip(4, 4, 4, 4), 5555).await;

    assert_net_ids_in_use(&server, 3).await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, TRIGGERS_CHANGED, "start.dot");
    drop(guard);

    server
}

/// Drop A's triggers entirely. TriggersChanged{A} tears down A→C.
/// A's ingress edge and D→C survive.
#[tokio::test]
async fn triggers_changed_remove_A_trigger() {
    let server = triggers_changed_setup().await;
    let new_config = load_config(TRIGGERS_CHANGED, "remove_A_trigger.toml").await;

    let mut guard = server.services().write().await;
    apply_config_update(&mut guard, new_config, server.orchestrator()).await;
    assert_graphviz(&guard, TRIGGERS_CHANGED, "after_remove_A_trigger.dot");
    assert!(stack_view(&guard)["A"].triggers().is_empty());
    drop(guard);

    assert_net_ids_in_use(&server, 2).await;
}

/// Swap A's edge C→D. TriggersChanged{A} tears down the existing A→C.
/// The new edge is not auto-built without fresh trigger traffic; other edges remain.
#[tokio::test]
async fn triggers_changed_swap_A_trigger() {
    let server = triggers_changed_setup().await;
    let new_config = load_config(TRIGGERS_CHANGED, "swap_A_trigger.toml").await;

    let mut guard = server.services().write().await;
    apply_config_update(&mut guard, new_config, server.orchestrator()).await;
    assert_graphviz(&guard, TRIGGERS_CHANGED, "after_swap_A_trigger.dot");
    assert_eq!(
        stack_view(&guard)["A"].triggers().get(&7777),
        Some(&"D".to_string())
    );
    drop(guard);

    assert_net_ids_in_use(&server, 2).await;
}

/// Add a second port to A's triggers. TriggersChanged still fires (map differs)
/// and tears down all of A's existing backend edges, even though the addition
/// didn't touch port 5555. The new port=7777 edge is not auto-built.
#[tokio::test]
async fn triggers_changed_add_A_trigger() {
    let server = triggers_changed_setup().await;
    let new_config = load_config(TRIGGERS_CHANGED, "add_A_trigger.toml").await;

    let mut guard = server.services().write().await;
    apply_config_update(&mut guard, new_config, server.orchestrator()).await;
    assert_graphviz(&guard, TRIGGERS_CHANGED, "after_add_A_trigger.dot");
    assert_eq!(stack_view(&guard)["A"].triggers().len(), 2);
    drop(guard);

    assert_net_ids_in_use(&server, 2).await;
}

/// Drop D's trigger. TriggersChanged{D} tears down D→C only.
/// A's ingress and backend edges are untouched.
#[tokio::test]
async fn triggers_changed_drop_D_trigger() {
    let server = triggers_changed_setup().await;
    let new_config = load_config(TRIGGERS_CHANGED, "drop_D_trigger.toml").await;

    let mut guard = server.services().write().await;
    apply_config_update(&mut guard, new_config, server.orchestrator()).await;
    assert_graphviz(&guard, TRIGGERS_CHANGED, "after_drop_D_trigger.dot");
    assert!(stack_view(&guard)["D"].triggers().is_empty());
    drop(guard);

    assert_net_ids_in_use(&server, 2).await;
}

// ===========================================================================
// backend_reachability_changed: A routes backend port 5555 to C, then loses its
// service declaration while C and an unrelated entry point remain configured.
// ===========================================================================

const BACKEND_REACHABILITY_CHANGED: &str = "backend_reachability_changed";

async fn backend_reachability_changed_setup() -> NullnetGrpcImpl {
    let services = load_fixture(BACKEND_REACHABILITY_CHANGED).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    let ip_map = HashMap::from([
        ("A", ip(1, 1, 1, 1)),
        ("C", ip(3, 3, 3, 3)),
        ("Z", ip(9, 9, 9, 9)),
    ]);
    register_services(&server, &ip_map, 8080).await;

    trigger_backend_edge(&server, "A", ip(1, 1, 1, 1), 5555).await;

    // A→C = 1 ID
    assert_net_ids_in_use(&server, 1).await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, BACKEND_REACHABILITY_CHANGED, "start.dot");
    drop(guard);

    server
}

/// A loses its service declaration, so A→C is torn down and A leaves the map.
#[tokio::test]
async fn backend_reachability_changed_lose_entry_point_A() {
    let server = backend_reachability_changed_setup().await;
    let new_config = load_config(BACKEND_REACHABILITY_CHANGED, "lose_entry_point_A.toml").await;

    let mut guard = server.services().write().await;
    apply_config_update(&mut guard, new_config, server.orchestrator()).await;
    assert_graphviz(
        &guard,
        BACKEND_REACHABILITY_CHANGED,
        "after_lose_entry_point_A.dot",
    );

    assert!(!stack_view(&guard).contains_key("A"));
    drop(guard);

    assert_net_ids_in_use(&server, 0).await;
}

// ===========================================================================
// backend_service_unregistered: A has co-located replicas a1 and a2 and routes
// backend port 5555 to C. B is unrelated. Tests selective replica/service
// removal while backend edges coexist with an ingress edge.
// ===========================================================================

const BACKEND_SERVICE_UNREGISTERED: &str = "backend_service_unregistered";

async fn backend_service_unregistered_setup() -> NullnetGrpcImpl {
    let services = load_fixture(BACKEND_SERVICE_UNREGISTERED).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    // B and C as standalone single-replica services
    let ip_map = HashMap::from([("B", ip(2, 2, 2, 2)), ("C", ip(3, 3, 3, 3))]);
    register_services(&server, &ip_map, 8080).await;

    // A: co-located replicas a1 and a2 at 1.1.1.1 (Docker Swarm)
    {
        let mut services = server.services().write().await;
        let a = stack_view_mut(&mut services)
            .get_mut("A")
            .expect("A in fixture");
        a.add_replica(ip(1, 1, 1, 1), 8080, Some("a1".into()));
        a.add_replica(ip(1, 1, 1, 1), 8080, Some("a2".into()));
    }
    server
        .orchestrator()
        .register_fake_client(ip(1, 1, 1, 1))
        .await;

    let proxy1 = ip(5, 5, 5, 5);
    server.orchestrator().register_fake_client(proxy1).await;

    // The ingress edge lands on a1 (first inserted, least-clients tie).
    setup_proxy_edge(&server, "A", proxy1, "10.0.0.1").await;
    // Backend edges are explicit per replica so both a1 and a2 initiate edges.
    setup_backend_edge_for_replica(&server, "A", ip(1, 1, 1, 1), Some("a1"), 5555).await;
    setup_backend_edge_for_replica(&server, "A", ip(1, 1, 1, 1), Some("a2"), 5555).await;

    assert_net_ids_in_use(&server, 3).await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, BACKEND_SERVICE_UNREGISTERED, "start.dot");
    drop(guard);

    server
}

/// Re-register host 1.1.1.1 with only a1 (drops a2). ReplicaRemoved{A,1.1.1.1,a2}
/// fires partial teardown: A(a2)→C is torn down via teardown_backend_edges.
/// A(a1)→C and the ingress edge (which landed on a1) survive.
#[tokio::test]
async fn backend_service_unregistered_drop_a2() {
    let server = backend_service_unregistered_setup().await;

    server
        .apply_services_list(ip(1, 1, 1, 1), &[("A".into(), 8080, Some("a1".into()))])
        .await
        .expect("apply_services_list failed");

    let guard = server.services().read().await;
    assert_graphviz(&guard, BACKEND_SERVICE_UNREGISTERED, "after_drop_a2.dot");

    let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] else {
        panic!("A should still be registered");
    };
    assert_eq!(reg_a.replicas().len(), 1, "only a1 should remain on A");
    assert_eq!(reg_a.replicas()[0].docker_container(), Some("a1"));

    // C should retain the A(a1)-keyed backend client; A(a2)'s entry is gone
    let ServiceInfo::Registered(reg_c) = &stack_view(&guard)["C"] else {
        panic!("C should be registered");
    };
    assert_eq!(reg_c.client_count(), 1, "only A(a1)→C should remain");
    drop(guard);

    assert_net_ids_in_use(&server, 2).await;
}

/// Removing unrelated service B does not disturb A's ingress or backend edges.
#[tokio::test]
async fn backend_service_unregistered_drop_B() {
    let server = backend_service_unregistered_setup().await;

    server
        .apply_services_list(ip(2, 2, 2, 2), &[])
        .await
        .expect("apply_services_list failed");

    let guard = server.services().read().await;
    assert_graphviz(&guard, BACKEND_SERVICE_UNREGISTERED, "after_drop_B.dot");

    assert!(matches!(
        stack_view(&guard)["B"],
        ServiceInfo::Unregistered(_)
    ));

    if let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] {
        assert_eq!(reg_a.client_count(), 1, "A's ingress edge should survive");
    }

    // Backend edges survive: C still has both A(a1) and A(a2) entries.
    let ServiceInfo::Registered(reg_c) = &stack_view(&guard)["C"] else {
        panic!("C should be registered");
    };
    assert_eq!(reg_c.client_count(), 2, "both A→C backend edges survive");
    drop(guard);

    assert_net_ids_in_use(&server, 3).await;
}

/// Re-register host 3.3.3.3 with empty list (drops C). C is_last →
/// C's backend edges are torn down, but A's independent ingress edge survives.
#[tokio::test]
async fn backend_service_unregistered_drop_C() {
    let server = backend_service_unregistered_setup().await;

    server
        .apply_services_list(ip(3, 3, 3, 3), &[])
        .await
        .expect("apply_services_list failed");

    let guard = server.services().read().await;
    assert_graphviz(&guard, BACKEND_SERVICE_UNREGISTERED, "after_drop_C.dot");

    assert!(matches!(
        stack_view(&guard)["C"],
        ServiceInfo::Unregistered(_)
    ));
    if let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] {
        assert_eq!(reg_a.client_count(), 1, "A's ingress edge should survive");
    }
    drop(guard);

    assert_net_ids_in_use(&server, 1).await;
}

// ===========================================================================
// backend_trigger_disambiguation: A entry-point with co-located replicas
// a1, a2 at 1.1.1.1, trigger 5555 → C. Drives handle_backend_trigger directly
// to exercise initiator-replica resolution by (ip, container) — the path the
// NFQUEUE client takes when it supplies the container name it resolved from
// the source IP. The other backend tests reach setup_backend_edge with an
// already-resolved replica, so this is the only coverage of the resolution
// `.find` itself.
// ===========================================================================

/// Topology only: A(a1, a2 @ 1.1.1.1), B, C registered, no edges pre-built so
/// each test owns the trigger that resolves the initiator replica.
async fn backend_disambiguation_setup() -> NullnetGrpcImpl {
    let services = load_fixture(BACKEND_SERVICE_UNREGISTERED).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    let ip_map = HashMap::from([("B", ip(2, 2, 2, 2)), ("C", ip(3, 3, 3, 3))]);
    register_services(&server, &ip_map, 8080).await;

    // A: co-located replicas a1 and a2 at 1.1.1.1 (Docker Swarm)
    {
        let mut services = server.services().write().await;
        let a = stack_view_mut(&mut services)
            .get_mut("A")
            .expect("A in fixture");
        a.add_replica(ip(1, 1, 1, 1), 8080, Some("a1".into()));
        a.add_replica(ip(1, 1, 1, 1), 8080, Some("a2".into()));
    }
    server
        .orchestrator()
        .register_fake_client(ip(1, 1, 1, 1))
        .await;

    server
}

/// A container name picks its own replica out of two co-located ones: a trigger
/// carrying "a2" must build the A(a2)→C edge and leave A(a1) untouched.
#[tokio::test]
async fn backend_trigger_disambiguates_colocated_replica_by_container() {
    let server = backend_disambiguation_setup().await;

    server
        .handle_backend_trigger("A", 5555, ip(1, 1, 1, 1), Some("a2"))
        .await
        .expect("backend trigger for a2 should succeed");

    let guard = server.services().read().await;
    let ServiceInfo::Registered(reg_c) = &stack_view(&guard)["C"] else {
        panic!("C should be registered");
    };
    let a2_client = crate::services::clients::Client::new_service(
        "A".to_string(),
        ip(1, 1, 1, 1),
        Some("a2".to_string()),
    );
    let a1_client = crate::services::clients::Client::new_service(
        "A".to_string(),
        ip(1, 1, 1, 1),
        Some("a1".to_string()),
    );
    assert!(
        reg_c.is_client_setup(&a2_client).is_some(),
        "A(a2)→C edge should be set up"
    );
    assert!(
        reg_c.is_client_setup(&a1_client).is_none(),
        "A(a1) must not have an edge — only a2 fired"
    );
    assert_eq!(reg_c.client_count(), 1, "exactly one initiator (a2) on C");
    drop(guard);

    // Only A(a2)→C exists.
    assert_net_ids_in_use(&server, 1).await;
}

/// A container name that matches no replica is a hard error — there is NO
/// fall back to IP-only when a container was supplied, otherwise co-located
/// replicas would be ambiguous again. Nothing is built.
#[tokio::test]
async fn backend_trigger_unknown_container_errors_without_ip_fallback() {
    let server = backend_disambiguation_setup().await;

    let result = server
        .handle_backend_trigger("A", 5555, ip(1, 1, 1, 1), Some("ghost"))
        .await;

    assert!(
        result.is_err(),
        "a container that matches no replica must not silently fall back to IP-only"
    );
    assert_net_ids_in_use(&server, 0).await;
}

/// No container (host process / pre-NFQUEUE caller) keeps the IP-only path:
/// the first replica on the sender IP is chosen.
#[tokio::test]
async fn backend_trigger_without_container_falls_back_to_ip_only() {
    let server = backend_disambiguation_setup().await;

    server
        .handle_backend_trigger("A", 5555, ip(1, 1, 1, 1), None)
        .await
        .expect("IP-only trigger should succeed");

    let guard = server.services().read().await;
    let ServiceInfo::Registered(reg_c) = &stack_view(&guard)["C"] else {
        panic!("C should be registered");
    };
    // a1 is inserted first, so the IP-only find resolves to it.
    let a1_client = crate::services::clients::Client::new_service(
        "A".to_string(),
        ip(1, 1, 1, 1),
        Some("a1".to_string()),
    );
    assert!(
        reg_c.is_client_setup(&a1_client).is_some(),
        "IP-only resolution should pick the first replica (a1)"
    );
    assert_eq!(reg_c.client_count(), 1);
    drop(guard);

    assert_net_ids_in_use(&server, 1).await;
}

// ===========================================================================
// backend_node_disconnected: same mixed topology as backend_service_unregistered
// (A co-located a1, a2 on 1.1.1.1, B on 2.2.2.2, C on 3.3.3.3, proxy1 on
// 5.5.5.5). Tests handle_node_disconnect's interaction with backend edges.
// ===========================================================================

const BACKEND_NODE_DISCONNECTED: &str = "backend_node_disconnected";

async fn backend_node_disconnected_setup() -> NullnetGrpcImpl {
    let services = load_fixture(BACKEND_NODE_DISCONNECTED).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    let ip_map = HashMap::from([("B", ip(2, 2, 2, 2)), ("C", ip(3, 3, 3, 3))]);
    register_services(&server, &ip_map, 8080).await;

    {
        let mut services = server.services().write().await;
        let a = stack_view_mut(&mut services)
            .get_mut("A")
            .expect("A in fixture");
        a.add_replica(ip(1, 1, 1, 1), 8080, Some("a1".into()));
        a.add_replica(ip(1, 1, 1, 1), 8080, Some("a2".into()));
    }
    server
        .orchestrator()
        .register_fake_client(ip(1, 1, 1, 1))
        .await;

    let proxy1 = ip(5, 5, 5, 5);
    server.orchestrator().register_fake_client(proxy1).await;

    setup_proxy_edge(&server, "A", proxy1, "10.0.0.1").await;
    setup_backend_edge_for_replica(&server, "A", ip(1, 1, 1, 1), Some("a1"), 5555).await;
    setup_backend_edge_for_replica(&server, "A", ip(1, 1, 1, 1), Some("a2"), 5555).await;

    assert_net_ids_in_use(&server, 3).await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, BACKEND_NODE_DISCONNECTED, "start.dot");
    drop(guard);

    server
}

/// Disconnect 1.1.1.1 (initiator host). Both a1 and a2 vanish at once →
/// ReplicasRemoved{A,1.1.1.1} with is_last=true → teardown_invalidated_service.
/// All edges torn down; A becomes Unregistered.
#[tokio::test]
async fn backend_node_disconnected_initiator_host() {
    let server = backend_node_disconnected_setup().await;

    server
        .orchestrator()
        .handle_node_disconnect(ip(1, 1, 1, 1), server.services())
        .await;

    let guard = server.services().read().await;
    assert_graphviz(
        &guard,
        BACKEND_NODE_DISCONNECTED,
        "after_disconnect_initiator.dot",
    );

    assert!(matches!(
        stack_view(&guard)["A"],
        ServiceInfo::Unregistered(_)
    ));
    if let ServiceInfo::Registered(reg_c) = &stack_view(&guard)["C"] {
        assert!(
            !reg_c.has_clients(),
            "C should have no backend clients left"
        );
    }
    drop(guard);

    assert_net_ids_in_use(&server, 0).await;
}

/// Disconnecting backend host C tears down A's backend edges while preserving
/// A's independent ingress edge.
#[tokio::test]
async fn backend_node_disconnected_backend_dep_host() {
    let server = backend_node_disconnected_setup().await;

    server
        .orchestrator()
        .handle_node_disconnect(ip(3, 3, 3, 3), server.services())
        .await;

    let guard = server.services().read().await;
    assert_graphviz(
        &guard,
        BACKEND_NODE_DISCONNECTED,
        "after_disconnect_backend_dep.dot",
    );

    assert!(matches!(
        stack_view(&guard)["C"],
        ServiceInfo::Unregistered(_)
    ));
    if let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] {
        assert_eq!(reg_a.client_count(), 1, "A's ingress edge should survive");
    }
    drop(guard);

    assert_net_ids_in_use(&server, 1).await;
}

/// Disconnect proxy host 5.5.5.5. Only ProxyDisconnected fires (no replicas
/// at that IP). The ingress edge is torn down; backend edges are untouched.
#[tokio::test]
async fn backend_node_disconnected_proxy_host() {
    let server = backend_node_disconnected_setup().await;

    server
        .orchestrator()
        .handle_node_disconnect(ip(5, 5, 5, 5), server.services())
        .await;

    let guard = server.services().read().await;
    assert_graphviz(
        &guard,
        BACKEND_NODE_DISCONNECTED,
        "after_disconnect_proxy.dot",
    );

    // A still registered with no proxy clients
    if let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] {
        assert!(!reg_a.has_clients(), "A should have no proxy clients");
    }
    // C still has both backend client entries
    let ServiceInfo::Registered(reg_c) = &stack_view(&guard)["C"] else {
        panic!("C should be registered");
    };
    assert_eq!(
        reg_c.client_count(),
        2,
        "both backend edges should survive proxy disconnect"
    );
    drop(guard);

    // The two backend edges survive.
    assert_net_ids_in_use(&server, 2).await;
}

// ===========================================================================
// backend_multi_replica: A, D, E are entry-points each backend-triggering to B.
// B has 3 replicas: b1, b2 on 2.2.2.2 (Docker Swarm), standalone on 4.4.4.4.
// Least-clients spreads edges: A→B(b1), D→B(4.4.4.4), E→B(b2). Tests the
// `only_through` filter in teardown_partial_replicas — partial removal of B's
// replicas tears down only edges routing through them.
// ===========================================================================

const BACKEND_MULTI_REPLICA: &str = "backend_multi_replica";

async fn backend_multi_replica_setup() -> NullnetGrpcImpl {
    let services = load_fixture(BACKEND_MULTI_REPLICA).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    // A, D, E single-replica entry points
    let ip_map = HashMap::from([
        ("A", ip(1, 1, 1, 1)),
        ("D", ip(5, 5, 5, 5)),
        ("E", ip(6, 6, 6, 6)),
    ]);
    register_services(&server, &ip_map, 8080).await;

    // B: 3 replicas — b1 (2.2.2.2), 4.4.4.4 standalone, b2 (2.2.2.2). Insertion
    // order matters for least-clients tie-breaking (`min_by_key` returns first).
    {
        let mut services = server.services().write().await;
        let b = stack_view_mut(&mut services)
            .get_mut("B")
            .expect("B in fixture");
        b.add_replica(ip(2, 2, 2, 2), 8080, Some("b1".into()));
        b.add_replica(ip(4, 4, 4, 4), 8080, None);
        b.add_replica(ip(2, 2, 2, 2), 8080, Some("b2".into()));
    }
    server
        .orchestrator()
        .register_fake_client(ip(2, 2, 2, 2))
        .await;
    server
        .orchestrator()
        .register_fake_client(ip(4, 4, 4, 4))
        .await;

    // Fire backend edges. Least-clients spreads: A→b1, D→4.4.4.4, E→b2.
    trigger_backend_edge(&server, "A", ip(1, 1, 1, 1), 5555).await;
    trigger_backend_edge(&server, "D", ip(5, 5, 5, 5), 5555).await;
    trigger_backend_edge(&server, "E", ip(6, 6, 6, 6), 5555).await;

    // Three backend edges use three NET IDs.
    assert_net_ids_in_use(&server, 3).await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, BACKEND_MULTI_REPLICA, "start.dot");

    // Sanity: each B replica should have exactly 1 backend client
    let ServiceInfo::Registered(reg_b) = &stack_view(&guard)["B"] else {
        panic!("B should be registered");
    };
    assert_eq!(reg_b.client_count(), 3);
    for replica in reg_b.replicas() {
        assert_eq!(
            replica.clients().len(),
            1,
            "each B replica should host exactly one backend edge"
        );
    }
    drop(guard);

    server
}

/// Disconnect 4.4.4.4 (B's standalone replica). ReplicasRemoved{B,4.4.4.4}
/// partial → teardown_partial_replicas finds D-keyed client on B's 4.4.4.4
/// replica and tears down D's edge through B (only_through=Some("B")).
/// A→B(b1) and E→B(b2) survive.
#[tokio::test]
async fn backend_multi_replica_disconnect_standalone_dep() {
    let server = backend_multi_replica_setup().await;

    server
        .orchestrator()
        .handle_node_disconnect(ip(4, 4, 4, 4), server.services())
        .await;

    let guard = server.services().read().await;
    assert_graphviz(
        &guard,
        BACKEND_MULTI_REPLICA,
        "after_disconnect_standalone_dep.dot",
    );

    let ServiceInfo::Registered(reg_b) = &stack_view(&guard)["B"] else {
        panic!("B should still be registered");
    };
    assert_eq!(reg_b.replicas().len(), 2, "b1 and b2 should remain");
    assert!(!reg_b.has_replica_on_ip(ip(4, 4, 4, 4)));
    assert_eq!(
        reg_b.client_count(),
        2,
        "A→B(b1) and E→B(b2) should survive"
    );
    drop(guard);

    // D→B freed; A→B and E→B survive = 2
    assert_net_ids_in_use(&server, 2).await;
}

/// Disconnect 2.2.2.2 (host of b1 and b2). ReplicasRemoved{B,2.2.2.2} partial.
/// teardown_partial_replicas finds A-keyed (on b1) and E-keyed (on b2) clients;
/// each upstream's edge through B is torn down via `only_through`.
/// D→B(4.4.4.4) survives untouched.
#[tokio::test]
async fn backend_multi_replica_disconnect_swarm_host() {
    let server = backend_multi_replica_setup().await;

    server
        .orchestrator()
        .handle_node_disconnect(ip(2, 2, 2, 2), server.services())
        .await;

    let guard = server.services().read().await;
    assert_graphviz(
        &guard,
        BACKEND_MULTI_REPLICA,
        "after_disconnect_swarm_host.dot",
    );

    let ServiceInfo::Registered(reg_b) = &stack_view(&guard)["B"] else {
        panic!("B should still be registered");
    };
    assert_eq!(reg_b.replicas().len(), 1, "only 4.4.4.4 should remain");
    assert!(reg_b.has_replica_on_ip(ip(4, 4, 4, 4)));
    assert_eq!(reg_b.client_count(), 1, "only D→B should survive");
    drop(guard);

    // A→B and E→B freed; D→B survives = 1
    assert_net_ids_in_use(&server, 1).await;
}

// ===========================================================================
// container suspend/resume: idle Docker-backed replicas are paused on
// registration (startup invariant); a proxy request resumes the selected
// replica before returning the upstream. Host replicas are never touched.
// ===========================================================================

fn count_msgs(
    log: &[NetMessage],
    suspend_container: Option<&str>,
    resume_container: Option<&str>,
) -> usize {
    log.iter()
        .filter(|m| match &m.message {
            Some(net_message::Message::ContainerSuspend(s)) => {
                suspend_container.is_some_and(|c| c == s.docker_container)
            }
            Some(net_message::Message::ContainerResume(r)) => {
                resume_container.is_some_and(|c| c == r.docker_container)
            }
            _ => false,
        })
        .count()
}

fn count_suspends(log: &[NetMessage], container: &str) -> usize {
    count_msgs(log, Some(container), None)
}

fn count_resumes(log: &[NetMessage], container: &str) -> usize {
    count_msgs(log, None, Some(container))
}

fn total_suspends(log: &[NetMessage]) -> usize {
    log.iter()
        .filter(|m| matches!(&m.message, Some(net_message::Message::ContainerSuspend(_))))
        .count()
}

fn replica_is_suspended(guard: &StackMap, service: &str, svc_ip: IpAddr, container: &str) -> bool {
    match stack_view(guard).get(service) {
        Some(ServiceInfo::Registered(reg)) => reg.replica_suspended(svc_ip, Some(container)),
        _ => false,
    }
}

/// Poll the recording log until `want` holds. The suspend command is
/// fire-and-forget (no ack to synchronize on), so we wait for it to drain.
/// Bounded so a genuine failure still terminates.
async fn wait_for_log(
    log: &std::sync::Arc<tokio::sync::Mutex<Vec<NetMessage>>>,
    want: impl Fn(&[NetMessage]) -> bool,
) {
    for _ in 0..200 {
        if want(&log.lock().await) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("log condition not met within timeout");
}

fn suspend_test_server() -> NullnetGrpcImpl {
    let mut inner = HashMap::new();
    // both are proxy entry points (timeout = Some); one Docker-backed, one host
    inner.insert(
        "svc".to_string(),
        ServiceInfo::new(
            HashMap::new(),
            Some(30),
            None,
            ServiceProtocol::Http,
            None,
            FilterPolicy::None,
            FilterPolicy::None,
        )
        .with_pausable(true),
    );
    inner.insert(
        "host_svc".to_string(),
        ServiceInfo::new(
            HashMap::new(),
            Some(30),
            None,
            ServiceProtocol::Http,
            None,
            FilterPolicy::None,
            FilterPolicy::None,
        )
        .with_pausable(true),
    );
    NullnetGrpcImpl::new_for_test(into_stack_map(inner))
}

fn declared_list(
    entries: Vec<(&str, u16, Option<&str>)>,
) -> HashMap<String, Vec<(String, Option<u16>, Option<String>)>> {
    HashMap::from([(
        TEST_STACK.to_string(),
        entries
            .into_iter()
            .map(|(n, p, d)| (n.to_string(), Some(p), d.map(ToString::to_string)))
            .collect(),
    )])
}

#[tokio::test]
async fn docker_replica_suspended_on_registration() {
    let server = suspend_test_server();
    let svc_ip = ip(1, 1, 1, 1);
    let log = server
        .orchestrator()
        .register_recording_client(svc_ip)
        .await;

    let list = declared_list(vec![
        ("svc", 8080, Some("svc_c1")),
        ("host_svc", 9090, None),
    ]);
    server
        .apply_services_list_by_stack(svc_ip, &list)
        .await
        .expect("services list apply failed");

    // the Docker-backed, never-requested replica is paused immediately
    {
        let guard = server.services().read().await;
        assert!(replica_is_suspended(&guard, "svc", svc_ip, "svc_c1"));
    }
    // exactly one suspend command was sent, only for the Docker-backed replica
    wait_for_log(&log, |l| count_suspends(l, "svc_c1") == 1).await;
    assert_eq!(total_suspends(&log.lock().await), 1);

    // re-declaration (the client's 10s heartbeat) does not re-suspend
    server
        .apply_services_list_by_stack(svc_ip, &list)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert_eq!(total_suspends(&log.lock().await), 1);
}

/// Backend involvement does not override the explicit pause policy.

#[tokio::test]
async fn backend_services_follow_pause_checkbox() {
    let mut inner = HashMap::new();
    // initiator declares a backend edge → dep (so it "has backend deps")
    inner.insert(
        "initiator".to_string(),
        ServiceInfo::new(
            HashMap::from([(8080u16, "dep".to_string())]),
            Some(30),
            None,
            ServiceProtocol::Http,
            None,
            FilterPolicy::None,
            FilterPolicy::None,
        ),
    );
    // dep is named by the backend edge (so it "is a backend dep")
    inner.insert(
        "dep".to_string(),
        ServiceInfo::new(
            HashMap::new(),
            None,
            None,
            ServiceProtocol::Http,
            None,
            FilterPolicy::None,
            FilterPolicy::None,
        ),
    );
    // a plain entry-point service with no backend involvement (control)
    inner.insert(
        "plain".to_string(),
        ServiceInfo::new(
            HashMap::new(),
            Some(30),
            None,
            ServiceProtocol::Http,
            None,
            FilterPolicy::None,
            FilterPolicy::None,
        ),
    );
    for name in ["dep", "plain"] {
        let info = inner.remove(name).unwrap();
        inner.insert(name.to_string(), info.with_pausable(true));
    }
    let server = NullnetGrpcImpl::new_for_test(into_stack_map(inner));

    let svc_ip = ip(1, 1, 1, 1);
    let log = server
        .orchestrator()
        .register_recording_client(svc_ip)
        .await;

    let list = declared_list(vec![
        ("initiator", 8080, Some("initiator_c1")),
        ("dep", 9090, Some("dep_c1")),
        ("plain", 7070, Some("plain_c1")),
    ]);
    server
        .apply_services_list_by_stack(svc_ip, &list)
        .await
        .expect("services list apply failed");

    {
        let guard = server.services().read().await;
        assert!(!replica_is_suspended(
            &guard,
            "initiator",
            svc_ip,
            "initiator_c1"
        ));
        assert!(replica_is_suspended(&guard, "dep", svc_ip, "dep_c1"));
        // control: the unrelated replica is still paused while idle
        assert!(replica_is_suspended(&guard, "plain", svc_ip, "plain_c1"));
    }

    // Both opted-in replicas pause; the unchecked initiator stays running.
    wait_for_log(&log, |l| count_suspends(l, "plain_c1") == 1).await;
    assert_eq!(count_suspends(&log.lock().await, "initiator_c1"), 0);
    assert_eq!(count_suspends(&log.lock().await, "dep_c1"), 1);
    assert_eq!(total_suspends(&log.lock().await), 2);
}

#[tokio::test]
async fn proxy_request_resumes_suspended_replica() {
    let server = suspend_test_server();
    let svc_ip = ip(1, 1, 1, 1);
    let proxy_ip = ip(5, 5, 5, 5);
    let log = server
        .orchestrator()
        .register_recording_client(svc_ip)
        .await;
    server.orchestrator().register_fake_client(proxy_ip).await;

    let list = declared_list(vec![("svc", 8080, Some("svc_c1"))]);
    server
        .apply_services_list_by_stack(svc_ip, &list)
        .await
        .unwrap();

    // precondition: suspended after registration
    {
        let guard = server.services().read().await;
        assert!(replica_is_suspended(&guard, "svc", svc_ip, "svc_c1"));
    }

    // a proxy request resumes it before returning the upstream
    server
        .handle_proxy_request("svc", proxy_ip, "10.0.0.1")
        .await
        .expect("proxy request failed");

    {
        let guard = server.services().read().await;
        assert!(
            !replica_is_suspended(&guard, "svc", svc_ip, "svc_c1"),
            "replica should be resumed once a client is attached"
        );
    }
    assert_eq!(count_resumes(&log.lock().await, "svc_c1"), 1);
}

// ===========================================================================
// trigger_scoping: a trigger port is watched host-wide, so the trigger config
// has to say which containers actually own it. Without that, every co-located
// container dialing the port is attributed to the declaring service, rejected
// by `handle_backend_trigger`, and its SYN dropped by the client.
// ===========================================================================

const TRIGGER_SCOPING: &str = "trigger_scoping";

fn declared_by_stack(
    entries: Vec<(&str, u16, Option<&str>)>,
) -> HashMap<String, Vec<(String, Option<u16>, Option<String>)>> {
    HashMap::from([(
        TEST_STACK.to_string(),
        entries
            .into_iter()
            .map(|(n, p, d)| (n.to_string(), Some(p), d.map(ToString::to_string)))
            .collect(),
    )])
}

#[tokio::test]
async fn trigger_carries_the_containers_that_own_it() {
    let services = load_fixture(TRIGGER_SCOPING).await;
    let declared = declared_by_stack(vec![
        ("S", 3000, Some("stack_s.1.abc")),
        ("P", 8932, Some("stack_p.1.xyz")),
    ]);

    let triggers = build_service_triggers(&services, &declared);

    assert_eq!(triggers.len(), 1, "only S declares a trigger");
    assert_eq!(triggers[0].service_name, "S");
    assert_eq!(triggers[0].ports, vec![8932]);
    assert_eq!(
        triggers[0].containers,
        vec!["stack_s.1.abc"],
        "P's container hosts the callee, not the trigger — it must not be listed, \
         or P's own traffic on 8932 would be attributed to S's trigger"
    );
}

/// Swarm: every replica of the declaring service on this node owns the trigger.
#[tokio::test]
async fn trigger_lists_every_colocated_replica() {
    let services = load_fixture(TRIGGER_SCOPING).await;
    let declared = declared_by_stack(vec![
        ("S", 3000, Some("stack_s.2.def")),
        ("S", 3000, Some("stack_s.1.abc")),
        ("S", 3000, Some("stack_s.1.abc")),
    ]);

    let triggers = build_service_triggers(&services, &declared);

    assert_eq!(triggers.len(), 1);
    assert_eq!(
        triggers[0].containers,
        vec!["stack_s.1.abc", "stack_s.2.def"],
        "sorted and de-duplicated"
    );
}

/// A bare-process service's trigger can never fire — the NFQUEUE path resolves
/// an initiator container and passes host traffic straight through. Shipping it
/// would claim the port with an owner matching every container instead of none,
/// which is exactly the bug being fixed.
#[tokio::test]
async fn bare_process_service_ships_no_trigger() {
    let services = load_fixture(TRIGGER_SCOPING).await;
    let declared = declared_by_stack(vec![("HostSvc", 4000, None)]);

    assert!(build_service_triggers(&services, &declared).is_empty());
}

/// A node hosting neither declares nothing.
#[tokio::test]
async fn node_hosting_no_trigger_service_gets_nothing() {
    let services = load_fixture(TRIGGER_SCOPING).await;
    let declared = declared_by_stack(vec![("P", 8932, Some("stack_p.1.xyz"))]);

    assert!(build_service_triggers(&services, &declared).is_empty());
}

// ===========================================================================
// backend_liveness: A --trigger 5555--> B, and no proxy anywhere in the
// picture. This is the autonomous `backend_trigger` shape from §2.5 of
// docs/uniform-edge-liveness-plan.md — the one case that inherits nothing.
// ===========================================================================

const BACKEND_LIVENESS: &str = "backend_liveness";

#[tokio::test]
async fn backend_reap_history_wait_releases_topology() {
    let server = NullnetGrpcImpl::new_for_test(load_fixture(BACKEND_LIVENESS).await);
    register_services(
        &server,
        &HashMap::from([("A", ip(1, 1, 1, 1)), ("B", ip(2, 2, 2, 2))]),
        8080,
    )
    .await;
    let db = attach_session_db(&server).await;
    server
        .handle_backend_trigger("A", 5555, ip(1, 1, 1, 1), None)
        .await
        .unwrap();
    server.orchestrator().sessions.flush().await;
    assert_eq!(db.sessions().count_active(TEST_STACK).await.unwrap(), 1);
    let key = ("A".to_string(), ip(1, 1, 1, 1), None, 5555);
    server
        .orchestrator()
        .set_backend_liveness(&key, false)
        .await;
    let db_guard = db.hold_connection().await;
    let reap = reap_idle_backend_edges(
        server.services(),
        server.orchestrator(),
        std::time::Duration::ZERO,
    );
    tokio::pin!(reap);
    assert!(futures::poll!(&mut reap).is_ready());
    assert!(
        server.services().try_read().is_ok(),
        "history close holds the global topology lock"
    );
    assert_net_ids_in_use(&server, 0).await;
    let task_server = server.clone();
    let replacement = tokio::spawn(async move {
        task_server
            .handle_backend_trigger("A", 5555, ip(1, 1, 1, 1), None)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !server.orchestrator().holds_backend_session(&key).await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(db_guard);
    replacement.await.unwrap().unwrap();
    assert_net_ids_in_use(&server, 1).await;
    server.orchestrator().sessions.flush().await;
    assert_eq!(db.sessions().count_active(TEST_STACK).await.unwrap(), 1);
    report_backend_idle_and_reap(&server, "A", key.1, 5555).await;
    assert_net_ids_in_use(&server, 0).await;
    server.orchestrator().sessions.flush().await;
    assert_eq!(db.sessions().count_active(TEST_STACK).await.unwrap(), 0);
}

#[tokio::test]
async fn backend_pending_history_allows_teardown_and_replacement() {
    let server = NullnetGrpcImpl::new_for_test(load_fixture(BACKEND_LIVENESS).await);
    register_services(
        &server,
        &HashMap::from([("A", ip(1, 1, 1, 1)), ("B", ip(2, 2, 2, 2))]),
        8080,
    )
    .await;
    let db = attach_session_db(&server).await;
    let db_guard = db.hold_connection().await;
    let key = ("A".to_string(), ip(1, 1, 1, 1), None, 5555);
    let task_server = server.clone();
    let mut first = tokio::spawn(async move {
        task_server
            .handle_backend_trigger("A", 5555, ip(1, 1, 1, 1), None)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !server.orchestrator().holds_backend_session(&key).await {
            tokio::task::yield_now().await;
        }
        server
            .orchestrator()
            .set_backend_liveness(&key, false)
            .await;
        while server
            .orchestrator()
            .nearest_backend_expiry(std::time::Duration::ZERO)
            .await
            .is_none()
        {
            tokio::task::yield_now().await;
        }
        reap_idle_backend_edges(
            server.services(),
            server.orchestrator(),
            std::time::Duration::ZERO,
        )
        .await;
    })
    .await
    .expect("history storage blocked topology or teardown");
    tokio::time::timeout(std::time::Duration::from_secs(1), &mut first)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_net_ids_in_use(&server, 0).await;
    let task_server = server.clone();
    let replacement = tokio::spawn(async move {
        task_server
            .handle_backend_trigger("A", 5555, ip(1, 1, 1, 1), None)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !server.orchestrator().holds_backend_session(&key).await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(db_guard);
    replacement.await.unwrap().unwrap();
    assert_net_ids_in_use(&server, 1).await;
    server.orchestrator().sessions.flush().await;
    assert_eq!(db.sessions().count_active(TEST_STACK).await.unwrap(), 1);
    report_backend_idle_and_reap(&server, "A", key.1, 5555).await;
    assert_net_ids_in_use(&server, 0).await;
    server.orchestrator().sessions.flush().await;
    assert_eq!(db.sessions().count_active(TEST_STACK).await.unwrap(), 0);
}

#[tokio::test]
async fn trigger_atomic_concurrent_backend_claims_release_once() {
    let server = NullnetGrpcImpl::new_for_test(load_fixture(BACKEND_LIVENESS).await);
    register_services(
        &server,
        &HashMap::from([("A", ip(1, 1, 1, 1)), ("B", ip(2, 2, 2, 2))]),
        8080,
    )
    .await;
    let (a, b) = tokio::join!(
        server.handle_backend_trigger("A", 5555, ip(1, 1, 1, 1), None),
        server.handle_backend_trigger("A", 5555, ip(1, 1, 1, 1), None),
    );
    a.unwrap();
    b.unwrap();
    let client = Client::new_service("A".into(), ip(1, 1, 1, 1), None);
    let holds = client_entry_holds(&*server.services().read().await, "B", &client);
    report_backend_idle_and_reap(&server, "A", ip(1, 1, 1, 1), 5555).await;
    assert_eq!(holds, Some(1), "duplicate trigger acquired two holds");
    assert_net_ids_in_use(&server, 0).await;
}

#[tokio::test]
async fn trigger_atomic_backend_idle_during_setup_is_preserved() {
    let server = std::sync::Arc::new(NullnetGrpcImpl::new_for_test(
        load_fixture(BACKEND_LIVENESS).await,
    ));
    register_services(
        &server,
        &HashMap::from([("A", ip(1, 1, 1, 1)), ("B", ip(2, 2, 2, 2))]),
        8080,
    )
    .await;
    let (log, gate) = server
        .orchestrator()
        .register_gated_client(ip(2, 2, 2, 2))
        .await;
    let task_server = server.clone();
    let build = tokio::spawn(async move {
        task_server
            .handle_backend_trigger("A", 5555, ip(1, 1, 1, 1), None)
            .await
    });
    wait_for_log(&log, |l| !setup_net_ids(l).is_empty()).await;
    server
        .orchestrator()
        .set_backend_liveness(&("A".into(), ip(1, 1, 1, 1), None, 5555), false)
        .await;
    gate.add_permits(64);
    build.await.unwrap().unwrap();
    {
        reap_idle_backend_edges(
            server.services(),
            server.orchestrator(),
            std::time::Duration::ZERO,
        )
        .await;
    }
    assert_net_ids_in_use(&server, 0).await;
}

#[tokio::test]
async fn trigger_atomic_backend_owner_removed_during_setup_unwinds() {
    let server = std::sync::Arc::new(NullnetGrpcImpl::new_for_test(
        load_fixture(BACKEND_LIVENESS).await,
    ));
    register_services(
        &server,
        &HashMap::from([("A", ip(1, 1, 1, 1)), ("B", ip(2, 2, 2, 2))]),
        8080,
    )
    .await;
    let (log, gate) = server
        .orchestrator()
        .register_gated_client(ip(2, 2, 2, 2))
        .await;
    let task_server = server.clone();
    let build = tokio::spawn(async move {
        task_server
            .handle_backend_trigger("A", 5555, ip(1, 1, 1, 1), None)
            .await
    });
    wait_for_log(&log, |l| !setup_net_ids(l).is_empty()).await;
    server
        .apply_services_list(ip(1, 1, 1, 1), &[])
        .await
        .unwrap();
    gate.add_permits(64);
    let _ = build.await.unwrap();
    assert_net_ids_in_use(&server, 0).await;
    assert!(
        !server
            .orchestrator()
            .holds_backend_session(&("A".into(), ip(1, 1, 1, 1), None, 5555))
            .await
    );
}

#[tokio::test]
async fn cancelled_backend_trigger_finishes_its_detached_setup() {
    let server = std::sync::Arc::new(NullnetGrpcImpl::new_for_test(
        load_fixture(BACKEND_LIVENESS).await,
    ));
    let source_ip = ip(1, 1, 1, 1);
    let target_ip = ip(2, 2, 2, 2);
    register_services(
        &server,
        &HashMap::from([("A", source_ip), ("B", target_ip)]),
        8080,
    )
    .await;
    let (target_log, gate) = server.orchestrator().register_gated_client(target_ip).await;

    let task_server = server.clone();
    let trigger = tokio::spawn(async move {
        task_server
            .handle_backend_trigger("A", 5555, source_ip, None)
            .await
    });
    wait_for_log(&target_log, |log| !setup_net_ids(log).is_empty()).await;
    trigger.abort();
    let _ = trigger.await;
    gate.add_permits(64);

    let client = Client::new_service("A".into(), source_ip, None);
    for _ in 0..400 {
        if client_entry_net_id(&*server.services().read().await, "B", &client)
            .is_some_and(|net_id| net_id != 0)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        client_entry_net_id(&*server.services().read().await, "B", &client)
            .is_some_and(|net_id| net_id != 0),
        "cancelled trigger abandoned its pending reservation"
    );
    assert!(
        server
            .orchestrator()
            .holds_backend_session(&("A".into(), source_ip, None, 5555))
            .await,
        "detached setup completed without recording its owner"
    );

    report_backend_idle_and_reap(&server, "A", source_ip, 5555).await;
    assert_net_ids_in_use(&server, 0).await;
}

#[tokio::test]
async fn backend_target_removed_mid_setup_unwinds_the_tunnel() {
    let server = std::sync::Arc::new(NullnetGrpcImpl::new_for_test(
        load_fixture(BACKEND_LIVENESS).await,
    ));
    let source_ip = ip(1, 1, 1, 1);
    let target_ip = ip(2, 2, 2, 2);
    let surviving_target_ip = ip(3, 3, 3, 3);
    register_services(
        &server,
        &HashMap::from([("A", source_ip), ("B", target_ip)]),
        8080,
    )
    .await;
    register_services(&server, &HashMap::from([("B", surviving_target_ip)]), 8080).await;
    let source_log = server
        .orchestrator()
        .register_recording_client(source_ip)
        .await;
    let (target_log, gate) = server.orchestrator().register_gated_client(target_ip).await;

    let task_server = server.clone();
    let build = tokio::spawn(async move {
        task_server
            .handle_backend_trigger("A", 5555, source_ip, None)
            .await
    });
    wait_for_log(&target_log, |log| !setup_net_ids(log).is_empty()).await;
    server.apply_services_list(target_ip, &[]).await.unwrap();
    gate.add_permits(64);

    assert!(build.await.unwrap().is_err());
    wait_for_log(&target_log, |log| !teardown_net_ids(log).is_empty()).await;
    let mut teardown_ids = teardown_net_ids(&target_log.lock().await);
    teardown_ids.extend(teardown_net_ids(&source_log.lock().await));
    assert!(
        teardown_ids.iter().all(|net_id| *net_id != 0),
        "pending edge dispatched a teardown for unallocated net id 0"
    );
    assert_net_ids_in_use(&server, 0).await;
    let client = Client::new_service("A".into(), source_ip, None);
    assert_eq!(
        client_entry_net_id(&*server.services().read().await, "B", &client),
        None,
        "removed target left a server-side edge record"
    );
    assert!(
        !server
            .orchestrator()
            .holds_backend_session(&("A".into(), source_ip, None, 5555))
            .await,
        "failed setup left a backend owner behind"
    );
}

async fn backend_liveness_setup() -> NullnetGrpcImpl {
    let services = load_fixture(BACKEND_LIVENESS).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    let ip_map = HashMap::from([("A", ip(1, 1, 1, 1)), ("B", ip(2, 2, 2, 2))]);
    register_services(&server, &ip_map, 8080).await;

    trigger_backend_edge(&server, "A", ip(1, 1, 1, 1), 5555).await;
    server
}

fn client_count_of(guard: &StackMap, name: &str) -> usize {
    match &stack_view(guard)[name] {
        ServiceInfo::Registered(reg) => reg.client_count(),
        ServiceInfo::Unregistered(_) => 0,
    }
}

/// Gate 1 for Step 4: an edge built by an autonomous trigger is reaped by
/// nothing. Both services carry `timeout = 1`, so the idle path is armed and
/// running — it simply never considers this edge, because
/// `expired_proxy_clients` filters on `Client::is_proxy` and a trigger-built
/// edge's client is a *service* client.
///
/// The failure this documents: the edge survives every idle pass forever, and
/// only a config change or the container dying takes it down.
#[tokio::test]
async fn autonomous_trigger_edge_is_never_reaped_on_idle() {
    let server = backend_liveness_setup().await;

    assert_eq!(
        client_count_of(&*server.services().read().await, "B"),
        1,
        "precondition: the trigger built A->B"
    );

    // Many multiples of the 1s timeout, with the idle pass running each time.
    for _ in 0..3 {
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        let mut guard = server.services().write().await;
        apply_timeouts(stack_view_mut(&mut guard), server.orchestrator(), "default").await;
    }

    assert_eq!(
        client_count_of(&*server.services().read().await, "B"),
        1,
        "the trigger-built edge outlives every idle pass: nothing reaps it"
    );
}

/// Report the client's "last connection on this trigger port closed", then run
/// the reap with a zero debounce so the test does not sleep out the real one.
async fn report_backend_idle_and_reap(
    server: &NullnetGrpcImpl,
    service: &str,
    ip: IpAddr,
    port: u16,
) {
    server
        .orchestrator()
        .set_backend_liveness(&(service.to_string(), ip, None, port), false)
        .await;
    reap_idle_backend_edges(
        server.services(),
        server.orchestrator(),
        std::time::Duration::ZERO,
    )
    .await;
}

/// The Step 4 path: once the initiator's connections on the trigger port are
/// provably gone, the edge it built is released and — nothing else holding it
/// — comes down. The idle timer plays no part; the client's conntrack view does.
#[tokio::test]
async fn trigger_edge_is_released_once_its_connections_close() {
    let server = backend_liveness_setup().await;
    assert_eq!(
        client_count_of(&*server.services().read().await, "B"),
        1,
        "precondition: the trigger built A->B"
    );

    report_backend_idle_and_reap(&server, "A", ip(1, 1, 1, 1), 5555).await;

    assert_eq!(
        client_count_of(&*server.services().read().await, "B"),
        0,
        "the last close releases the only hold, so the edge comes down"
    );
    assert_net_ids_in_use(&server, 0).await;
}

/// A close for an edge we hold no refcount on must not decrement anything.
///
/// This is what keeps the liveness signal from consuming an increment it never
/// contributed — an edge already present for another reason, or one whose hold a
/// config-change teardown already released. `set_backend_liveness` drops the
/// report rather than inventing a session for it.
#[tokio::test]
async fn a_close_without_a_held_refcount_decrements_nothing() {
    let server = backend_liveness_setup().await;
    // Build A->B, then take our hold away exactly as a teardown from another
    // path would, leaving the edge up but unheld.
    server
        .orchestrator()
        .forget_backend_sessions("A", ip(1, 1, 1, 1), None, &[5555])
        .await;

    report_backend_idle_and_reap(&server, "A", ip(1, 1, 1, 1), 5555).await;

    assert_eq!(
        client_count_of(&*server.services().read().await, "B"),
        1,
        "no session means no hold to release, so the edge is untouched"
    );
}

/// Reaping twice in a row releases once: taking the session out of the map is
/// what bounds it, not the caller remembering to only ask once.
#[tokio::test]
async fn a_second_reap_pass_releases_nothing_more() {
    let server = backend_liveness_setup().await;
    // A->B built by the trigger, and D->B built independently, so B keeps a
    // client entry after A's release and a stray extra decrement would show up
    // as that entry vanishing too.
    let ip_map = HashMap::from([("D", ip(4, 4, 4, 4))]);
    register_services(&server, &ip_map, 8080).await;
    trigger_backend_edge(&server, "D", ip(4, 4, 4, 4), 5555).await;
    assert_eq!(
        client_count_of(&*server.services().read().await, "B"),
        2,
        "precondition: A->B and D->B are separate client entries"
    );

    server
        .orchestrator()
        .set_backend_liveness(&("A".to_string(), ip(1, 1, 1, 1), None, 5555), false)
        .await;
    for _ in 0..3 {
        reap_idle_backend_edges(
            server.services(),
            server.orchestrator(),
            std::time::Duration::ZERO,
        )
        .await;
    }

    assert_eq!(
        client_count_of(&*server.services().read().await, "B"),
        1,
        "A's hold is released once; repeated passes must not touch D's"
    );
}

/// A trigger whose edge cannot be built takes no increment, so it must record
/// no hold. A phantom hold is worse than useless: it makes the later
/// `claim_backend_session` a no-op, so the trigger rides on an increment
/// something else contributed and its close consumes that one instead.
///
/// `G` triggers into `Ghost`, which no fixture registers, so
/// backend edge setup bails.
#[tokio::test]
async fn a_bailed_setup_records_no_hold() {
    let server = backend_liveness_setup().await;
    let ip_map = HashMap::from([("G", ip(7, 7, 7, 7))]);
    register_services(&server, &ip_map, 8080).await;

    trigger_backend_edge(&server, "G", ip(7, 7, 7, 7), 7777).await;

    assert!(
        !server
            .orchestrator()
            .holds_backend_session(&("G".to_string(), ip(7, 7, 7, 7), None, 7777))
            .await,
        "a setup that built nothing must not claim to hold a refcount"
    );
    // Contrast: the trigger that did build one does hold it.
    assert!(
        server
            .orchestrator()
            .holds_backend_session(&("A".to_string(), ip(1, 1, 1, 1), None, 5555))
            .await,
        "a setup that built the edge holds exactly one refcount"
    );
}

// ===========================================================================
// Session history — the ingress half, through the real setup/teardown path.
//
// Every other test in this file runs with no DB attached, so `SessionStore`
// no-ops and `open_ingress`/`close_ingress` never actually write. These attach
// one, so the two ingress call sites are exercised for real rather than only
// at the repository level.
// ===========================================================================

async fn attach_session_db(server: &NullnetGrpcImpl) -> crate::db::Db {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "nullnet-server-ingress-history-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = crate::db::Db::open(dir.join("test.db").to_str().unwrap())
        .await
        .unwrap();
    server.orchestrator().sessions.attach_db(db.clone());
    db
}

async fn session_rows(server: &NullnetGrpcImpl, db: &crate::db::Db) -> Vec<crate::db::SessionRow> {
    server.orchestrator().sessions.flush().await;
    db.sessions()
        .query(TEST_STACK, None, None, None, None, None, None, None, 50)
        .await
        .unwrap()
}

/// A proxy request writes an ingress row; letting it time out closes that row.
/// Asserts the row's contents too — the stack, service and client IP are all
/// resolved on the setup path, so a wrong one would be invisible to any test
/// that only counted rows.
#[tokio::test]
async fn an_ingress_session_opens_and_closes_a_row() {
    let services = load_fixture(MAX_NETWORKS).await;
    let server = NullnetGrpcImpl::new_for_test(services);
    let db = attach_session_db(&server).await;

    let ip_map = HashMap::from([("A", ip(1, 1, 1, 1)), ("B", ip(2, 2, 2, 2))]);
    let proxy = ip(5, 5, 5, 5);
    register_services(&server, &ip_map, 8080).await;
    server.orchestrator().register_fake_client(proxy).await;

    setup_proxy_edge(&server, "A", proxy, "10.0.0.1").await;

    // Exactly one row is written for the ingress edge.
    let rows = session_rows(&server, &db).await;
    assert_eq!(rows.len(), 1, "expected one ingress row, got {rows:?}");
    let row = &rows[0];
    assert_eq!(row.direction, "ingress");
    assert_eq!(row.stack, TEST_STACK);
    assert_eq!(row.service, "A");
    assert_eq!(row.peer_ip, "10.0.0.1");
    assert!(row.ended_at.is_none(), "should still be live");
    let detail: serde_json::Value = serde_json::from_str(&row.detail).unwrap();
    assert!(
        detail["client_net"].is_string() && detail["server_net"].is_string(),
        "overlay addresses should be recorded: {detail}"
    );
    let net_id = row.net_id;

    // Time it out; the teardown path must close the row it opened.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let mut guard = server.services().write().await;
    apply_timeouts(
        stack_view_mut(&mut guard),
        server.orchestrator(),
        TEST_STACK,
    )
    .await;
    drop(guard);

    let rows = session_rows(&server, &db).await;
    assert_eq!(rows.len(), 1, "closing must not add a second row");
    assert_eq!(rows[0].net_id, net_id);
    assert!(rows[0].ended_at.is_some(), "should have been closed");
}

/// The standard client and each overflow group have separate sessions.
/// Expiring either must close exactly its own row.
#[tokio::test]
async fn max_networks_overflow_closes_each_client_session() {
    let services = load_fixture(MAX_NETWORKS).await;
    let server = NullnetGrpcImpl::new_for_test(services);
    let db = attach_session_db(&server).await;

    let ip_map = HashMap::from([("A", ip(1, 1, 1, 1)), ("B", ip(2, 2, 2, 2))]);
    let proxy = ip(5, 5, 5, 5);
    register_services(&server, &ip_map, 8080).await;
    server.orchestrator().register_fake_client(proxy).await;

    // Two clients, 2s apart so the first is comfortably expired while the
    // second is comfortably alive (same margins as max_networks_reuse_lifecycle
    // — timeouts use real `Instant`, not tokio's pausable clock).
    setup_proxy_edge(&server, "A", proxy, "10.0.0.1").await;
    tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
    setup_proxy_edge(&server, "A", proxy, "10.0.0.2").await;

    let rows = session_rows(&server, &db).await;
    assert_eq!(rows.len(), 2, "one row per client, got {rows:?}");
    let net_ids: HashSet<i32> = rows.iter().map(|r| r.net_id).collect();
    assert_eq!(net_ids.len(), 2, "overflow has its own group");
    assert!(rows.iter().all(|r| r.ended_at.is_none()));

    // First client expires while the overflow client remains active.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    crate::timeout::reap_ingress_timeouts(server.services(), server.orchestrator()).await;

    let rows = session_rows(&server, &db).await;
    let closed: Vec<_> = rows.iter().filter(|r| r.ended_at.is_some()).collect();
    assert_eq!(closed.len(), 1, "only the expired client's row closes");
    assert_eq!(closed[0].peer_ip, "10.0.0.1");

    // Second client expires; now everything is closed and nothing lingers.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    crate::timeout::reap_ingress_timeouts(server.services(), server.orchestrator()).await;

    let rows = session_rows(&server, &db).await;
    assert_eq!(rows.len(), 2, "no extra rows appeared");
    assert!(
        rows.iter().all(|r| r.ended_at.is_some()),
        "every client's row must close: {rows:?}"
    );
}

#[tokio::test]
async fn pausable_defaults_off_and_live_toggle_resumes() {
    let server = suspend_test_server();
    let svc_ip = ip(1, 1, 1, 1);
    let log = server
        .orchestrator()
        .register_recording_client(svc_ip)
        .await;
    let list = declared_list(vec![("svc", 8080, Some("svc_c1"))]);
    server
        .apply_services_list_by_stack(svc_ip, &list)
        .await
        .unwrap();
    assert!(replica_is_suspended(
        &*server.services().read().await,
        "svc",
        svc_ip,
        "svc_c1"
    ));
    {
        let mut guard = server.services().write().await;
        let info = guard.get_mut(TEST_STACK).unwrap().remove("svc").unwrap();
        guard
            .get_mut(TEST_STACK)
            .unwrap()
            .insert("svc".into(), info.with_pausable(false));
        crate::services::service_info::reconcile_container_pauses(
            &mut guard,
            server.orchestrator(),
        )
        .await;
        let ServiceInfo::Registered(reg) = &guard[TEST_STACK]["svc"] else {
            unreachable!()
        };
        let pending = reg.replicas()[0].resume(server.orchestrator());
        drop(guard);
        if let Some(pending) = pending {
            assert!(pending.wait().await);
        }
        assert!(!replica_is_suspended(
            &*server.services().read().await,
            "svc",
            svc_ip,
            "svc_c1"
        ));
    }
    assert_eq!(count_suspends(&log.lock().await, "svc_c1"), 1);
    server
        .apply_services_list_by_stack(svc_ip, &list)
        .await
        .unwrap();
    assert_eq!(count_suspends(&log.lock().await, "svc_c1"), 1);
}

#[tokio::test]
async fn pausable_backend_source_resumes_and_stays_running_while_held() {
    let (server, node, log) = paused_backend_test_server().await;
    server
        .handle_backend_trigger("source", 80, node, Some("source_c"))
        .await
        .unwrap();
    let mut guard = server.services().write().await;
    crate::services::service_info::reconcile_container_pauses(&mut guard, server.orchestrator())
        .await;
    assert!(!replica_is_suspended(&guard, "source", node, "source_c"));
    assert!(!replica_is_suspended(&guard, "dep", node, "dep_c"));
    assert_eq!(count_suspends(&log.lock().await, "source_c"), 1);
}

#[tokio::test]
async fn pausable_shared_container_requires_all_declarations_to_opt_in() {
    let (inner, _, _) = crate::services::input::validate_stack_toml(
        r#"
[[services]]
name = "opted"
docker_container = "shared"
port = 80
pausable = true
[[services]]
name = "unchecked"
docker_container = "shared"
port = 81
"#,
    )
    .unwrap();
    let server = NullnetGrpcImpl::new_for_test(into_stack_map(inner));
    let node = ip(1, 1, 1, 1);
    let log = server.orchestrator().register_recording_client(node).await;
    server
        .apply_services_list_by_stack(
            node,
            &declared_list(vec![
                ("opted", 80, Some("shared")),
                ("unchecked", 81, Some("shared")),
            ]),
        )
        .await
        .unwrap();
    assert_eq!(total_suspends(&log.lock().await), 0);
}

#[tokio::test]
async fn pausable_survives_last_replica_disappearing() {
    let server = suspend_test_server();
    let node = ip(1, 1, 1, 1);
    let log = server.orchestrator().register_recording_client(node).await;
    let list = declared_list(vec![("svc", 8080, Some("svc_c1"))]);
    server
        .apply_services_list_by_stack(node, &list)
        .await
        .unwrap();
    server
        .apply_services_list_by_stack(node, &HashMap::new())
        .await
        .unwrap();
    assert!(stack_view(&*server.services().read().await)["svc"].pausable());
    server
        .apply_services_list_by_stack(node, &list)
        .await
        .unwrap();
    assert!(replica_is_suspended(
        &*server.services().read().await,
        "svc",
        node,
        "svc_c1"
    ));
    wait_for_log(&log, |l| count_suspends(l, "svc_c1") == 2).await;
}

#[tokio::test]
async fn pausable_unacknowledged_policy_resume_does_not_lock_services() {
    let server = suspend_test_server();
    let node = ip(1, 1, 1, 1);
    server.orchestrator().register_fake_client(node).await;
    server
        .apply_services_list_by_stack(node, &declared_list(vec![("svc", 8080, Some("svc_c1"))]))
        .await
        .unwrap();
    let (log, gate) = server.orchestrator().register_gated_client(node).await;
    let worker = server.clone();
    let task = tokio::spawn(async move {
        let mut guard = worker.services().write().await;
        let info = guard.get_mut(TEST_STACK).unwrap().remove("svc").unwrap();
        guard
            .get_mut(TEST_STACK)
            .unwrap()
            .insert("svc".into(), info.with_pausable(false));
        crate::services::service_info::reconcile_container_pauses(
            &mut guard,
            worker.orchestrator(),
        )
        .await;
    });
    wait_for_log(&log, |l| count_resumes(l, "svc_c1") == 1).await;
    let unlocked = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        server.services().write(),
    )
    .await
    .is_ok();
    task.await.unwrap();
    assert!(
        unlocked,
        "a withheld resume ack blocked unrelated service operations"
    );
    let pending = {
        let mut guard = server.services().write().await;
        crate::services::service_info::reconcile_container_pauses(
            &mut guard,
            server.orchestrator(),
        )
        .await;
        let ServiceInfo::Registered(reg) = &guard[TEST_STACK]["svc"] else {
            unreachable!()
        };
        assert!(reg.replicas()[0].suspended());
        reg.replicas()[0].resume(server.orchestrator()).unwrap()
    };
    gate.add_permits(10);
    assert!(pending.wait().await);
    assert!(!replica_is_suspended(
        &*server.services().read().await,
        "svc",
        node,
        "svc_c1"
    ));
    assert_eq!(count_resumes(&log.lock().await, "svc_c1"), 1);
}

async fn paused_backend_test_server() -> (
    NullnetGrpcImpl,
    IpAddr,
    std::sync::Arc<tokio::sync::Mutex<Vec<NetMessage>>>,
) {
    let (inner, _, _) = crate::services::input::validate_stack_toml(
        r#"
[[services]]
name = "source"
docker_container = "source_c"
port = 80
pausable = true
backends = ["dep"]
[[services]]
name = "dep"
docker_container = "dep_c"
port = 80
pausable = true
"#,
    )
    .unwrap();
    let server = NullnetGrpcImpl::new_for_test(into_stack_map(inner));
    let node = ip(1, 1, 1, 1);
    let log = server.orchestrator().register_recording_client(node).await;
    server
        .apply_services_list_by_stack(
            node,
            &declared_list(vec![
                ("source", 80, Some("source_c")),
                ("dep", 80, Some("dep_c")),
            ]),
        )
        .await
        .unwrap();
    (server, node, log)
}

#[tokio::test]
async fn pausable_unacknowledged_trigger_resume_does_not_lock_services() {
    let (server, node, _) = paused_backend_test_server().await;
    let (log, gate) = server.orchestrator().register_gated_client(node).await;
    let worker = server.clone();
    let task = tokio::spawn(async move {
        worker
            .handle_backend_trigger("source", 80, node, Some("source_c"))
            .await
    });
    wait_for_log(&log, |l| count_resumes(l, "source_c") == 1).await;
    let unlocked = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        server.services().write(),
    )
    .await
    .is_ok();
    gate.add_permits(20);
    task.await.unwrap().unwrap();
    assert!(
        unlocked,
        "a withheld trigger resume ack blocked unrelated service operations"
    );
    assert_eq!(count_resumes(&log.lock().await, "source_c"), 1);
}

#[tokio::test]
async fn pausable_shared_container_does_not_pause_during_alias_resume() {
    let (inner, _, _) = crate::services::input::validate_stack_toml(
        r#"
[[services]]
name = "first"
docker_container = "shared"
port = 80
pausable = true
[[services]]
name = "second"
docker_container = "shared"
port = 81
pausable = true
"#,
    )
    .unwrap();
    let server = NullnetGrpcImpl::new_for_test(into_stack_map(inner));
    let node = ip(1, 1, 1, 1);
    server.orchestrator().register_fake_client(node).await;
    server
        .apply_services_list_by_stack(
            node,
            &declared_list(vec![
                ("first", 80, Some("shared")),
                ("second", 81, Some("shared")),
            ]),
        )
        .await
        .unwrap();
    let pending = {
        let guard = server.services().read().await;
        let ServiceInfo::Registered(reg) = &guard[TEST_STACK]["first"] else {
            unreachable!()
        };
        reg.replicas()[0].resume(server.orchestrator()).unwrap()
    };
    assert!(pending.wait().await);
    let (log, gate) = server.orchestrator().register_gated_client(node).await;
    let pending = {
        let guard = server.services().read().await;
        let ServiceInfo::Registered(reg) = &guard[TEST_STACK]["second"] else {
            unreachable!()
        };
        reg.replicas()[0].resume(server.orchestrator()).unwrap()
    };
    wait_for_log(&log, |l| count_resumes(l, "shared") == 1).await;
    let remained_running = {
        let mut guard = server.services().write().await;
        crate::services::service_info::reconcile_container_pauses(
            &mut guard,
            server.orchestrator(),
        )
        .await;
        !replica_is_suspended(&guard, "first", node, "shared")
    };
    gate.add_permits(20);
    assert!(pending.wait().await);
    assert!(
        remained_running,
        "an idle alias paused a container while its resume was pending"
    );
}

#[tokio::test]
async fn backend_peer_does_not_expand_its_backends() {
    let config: ServicesToml = toml::from_str(
        r#"
[[services]]
name = "source"
backends = ["peer"]
[[services]]
name = "peer"
port = 5555
timeout = 0
backends = ["backend-dep"]
[[services]]
name = "backend-dep"
port = 6666
"#,
    )
    .unwrap();
    let server = NullnetGrpcImpl::new_for_test(into_stack_map(config.services_map().unwrap()));
    let source_ip = ip(1, 1, 1, 1);
    register_services(
        &server,
        &HashMap::from([("source", source_ip), ("peer", ip(2, 2, 2, 2))]),
        8080,
    )
    .await;
    trigger_backend_edge(&server, "source", source_ip, 5555).await;
    assert_net_ids_in_use(&server, 1).await;
    {
        let guard = server.services().read().await;
        let services = stack_view(&guard);
        let ServiceInfo::Registered(peer) = &services["peer"] else {
            panic!("peer must be registered");
        };
        assert!(
            peer.client_net_id(&Client::new_service("source".into(), source_ip, None))
                .is_some()
        );
        assert!(matches!(
            &services["backend-dep"],
            ServiceInfo::Unregistered(_)
        ));
    }
    server
        .orchestrator()
        .set_backend_liveness(&("source".into(), source_ip, None, 5555), false)
        .await;
    reap_idle_backend_edges(
        server.services(),
        server.orchestrator(),
        std::time::Duration::ZERO,
    )
    .await;
    assert_net_ids_in_use(&server, 0).await;
}

#[tokio::test]
async fn proxy_request_does_not_open_declared_backends() {
    let config: ServicesToml = toml::from_str(
        r#"
[[services]]
name = "entry"
port = 8080
timeout = 0
backends = ["peer"]

[[services]]
name = "peer"
port = 5555
"#,
    )
    .unwrap();
    let server = NullnetGrpcImpl::new_for_test(into_stack_map(config.services_map().unwrap()));
    let entry_ip = ip(1, 1, 1, 1);
    let peer_ip = ip(2, 2, 2, 2);
    let proxy_ip = ip(3, 3, 3, 3);
    register_services(
        &server,
        &HashMap::from([("entry", entry_ip), ("peer", peer_ip)]),
        8080,
    )
    .await;
    server.orchestrator().register_fake_client(proxy_ip).await;

    setup_proxy_edge(&server, "entry", proxy_ip, "10.0.0.1").await;

    assert_net_ids_in_use(&server, 1).await;
    let guard = server.services().read().await;
    let ServiceInfo::Registered(entry) = &stack_view(&guard)["entry"] else {
        panic!("entry must be registered");
    };
    let ServiceInfo::Registered(peer) = &stack_view(&guard)["peer"] else {
        panic!("peer must be registered");
    };
    assert_eq!(entry.client_count(), 1);
    assert_eq!(peer.client_count(), 0);
}

#[tokio::test]
async fn changing_backend_peer_port_releases_and_rebuilds_the_session() {
    let text = r#"
[[services]]
name = "source"
backends = ["peer"]
[[services]]
name = "peer"
port = 5555
"#;
    let parsed: ServicesToml = toml::from_str(text).unwrap();
    let server = NullnetGrpcImpl::new_for_test(into_stack_map(parsed.services_map().unwrap()));
    let path =
        std::env::temp_dir().join(format!("nullnet-backend-port-{}.db", uuid::Uuid::new_v4()));
    let db = crate::db::Db::open(path.to_str().unwrap()).await.unwrap();
    server.orchestrator().sessions.attach_db(db.clone());
    let source_ip = ip(1, 1, 1, 1);
    register_services(
        &server,
        &HashMap::from([("source", source_ip), ("peer", ip(2, 2, 2, 2))]),
        5555,
    )
    .await;
    trigger_backend_edge(&server, "source", source_ip, 5555).await;
    assert_net_ids_in_use(&server, 1).await;
    server.orchestrator().sessions.flush().await;
    assert_eq!(db.sessions().count_active(TEST_STACK).await.unwrap(), 1);
    let updated: ServicesToml = toml::from_str(&text.replace("5555", "6666")).unwrap();
    {
        let mut guard = server.services().write().await;
        apply_config_update(
            &mut guard,
            into_stack_map(updated.services_map().unwrap()),
            server.orchestrator(),
        )
        .await;
        assert_eq!(
            stack_view(&guard)["source"].triggers().get(&6666),
            Some(&"peer".to_string())
        );
        assert!(!stack_view(&guard)["source"].triggers().contains_key(&5555));
    }
    assert_net_ids_in_use(&server, 0).await;
    server.orchestrator().sessions.flush().await;
    assert_eq!(db.sessions().count_active(TEST_STACK).await.unwrap(), 0);
    register_services(&server, &HashMap::from([("peer", ip(2, 2, 2, 2))]), 6666).await;
    trigger_backend_edge(&server, "source", source_ip, 6666).await;
    assert_net_ids_in_use(&server, 1).await;
    server.orchestrator().sessions.flush().await;
    assert_eq!(db.sessions().count_active(TEST_STACK).await.unwrap(), 1);
    let rows = db
        .sessions()
        .query(
            TEST_STACK,
            Some("backend"),
            None,
            None,
            None,
            None,
            None,
            None,
            10,
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    report_backend_idle_and_reap(&server, "source", source_ip, 6666).await;
    assert_net_ids_in_use(&server, 0).await;
    server.orchestrator().sessions.flush().await;
    assert_eq!(db.sessions().count_active(TEST_STACK).await.unwrap(), 0);
}

#[tokio::test]
async fn backend_packets_are_released_only_after_both_setup_acks() {
    let services = load_fixture(BACKEND_LIVENESS).await;
    let server = NullnetGrpcImpl::new_for_test(services);
    register_services(
        &server,
        &HashMap::from([("A", ip(1, 1, 1, 1)), ("B", ip(2, 2, 2, 2))]),
        8080,
    )
    .await;
    let initiator = server
        .orchestrator()
        .register_recording_client(ip(1, 1, 1, 1))
        .await;
    let (receiver, gate) = server
        .orchestrator()
        .register_gated_client(ip(2, 2, 2, 2))
        .await;
    let task_server = server.clone();
    let build = tokio::spawn(async move {
        task_server
            .handle_backend_trigger("A", 5555, ip(1, 1, 1, 1), None)
            .await
    });
    wait_for_log(&receiver, |messages| !setup_net_ids(messages).is_empty()).await;
    assert!(
        !initiator
            .lock()
            .await
            .iter()
            .any(|message| matches!(message.message, Some(net_message::Message::NetReady(_))))
    );
    gate.add_permits(1);
    build.await.unwrap().unwrap();
    let messages = initiator.lock().await;
    let setup = setup_net_ids(&messages);
    let ready: Vec<_> = messages
        .iter()
        .filter_map(|message| match &message.message {
            Some(net_message::Message::NetReady(ready)) => Some(ready.net_id),
            _ => None,
        })
        .collect();
    assert_eq!(ready.len(), 1);
    assert!(setup.contains(&ready[0]));
}
