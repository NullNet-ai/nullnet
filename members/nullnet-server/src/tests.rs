#![allow(non_snake_case)]

use crate::graphviz::render_graphviz;
use crate::nullnet_grpc_impl::NullnetGrpcImpl;
use crate::services::input::{ServicesToml, StackMap, apply_config_update};
use crate::services::service_info::ServiceInfo;
use crate::timeout::apply_timeouts;
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

async fn setup_proxy_chain(
    server: &NullnetGrpcImpl,
    service_name: &str,
    proxy_ip: IpAddr,
    client_ip: &str,
) {
    server
        .handle_proxy_request(service_name, proxy_ip, client_ip)
        .await
        .expect("proxy request failed");
}

/// Trigger the backend chain at `port` from `initiator_ip` (acting as the
/// announcing host). Mirrors the gRPC `BackendTrigger` entry point.
async fn trigger_backend_chain(
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

/// Set up a backend chain initiated by a specific replica (ip + optional
/// docker container). Used when co-located replicas need independent chains.
async fn setup_backend_chain_for_replica(
    server: &NullnetGrpcImpl,
    initiator_name: &str,
    initiator_ip: IpAddr,
    initiator_docker: Option<&str>,
    port: u16,
) {
    server
        .setup_backend_chain(
            TEST_STACK,
            initiator_name,
            initiator_ip,
            initiator_docker,
            port,
        )
        .await
        .expect("setup_backend_chain failed");
}

// ===========================================================================
// service_removed: A→C→D, B→D (D shared). proxy1→A+B, proxy2→A
// ===========================================================================

const SERVICE_REMOVED: &str = "service_removed";

async fn service_removed_setup() -> NullnetGrpcImpl {
    let services = load_fixture(SERVICE_REMOVED).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    let ip_map = HashMap::from([
        ("A", ip(1, 1, 1, 1)),
        ("B", ip(2, 2, 2, 2)),
        ("C", ip(3, 3, 3, 3)),
        ("D", ip(4, 4, 4, 4)),
    ]);
    let proxy1 = ip(5, 5, 5, 5);
    let proxy2 = ip(6, 6, 6, 6);
    register_services(&server, &ip_map, 8080).await;
    server.orchestrator().register_fake_client(proxy1).await;
    server.orchestrator().register_fake_client(proxy2).await;

    setup_proxy_chain(&server, "A", proxy1, "10.0.0.1").await;
    setup_proxy_chain(&server, "B", proxy1, "10.0.0.1").await;
    setup_proxy_chain(&server, "A", proxy2, "10.0.0.2").await;

    // A→C, C→D, proxy1→A, B→D, proxy1→B, proxy2→A = 6 IDs
    assert_net_ids_in_use(&server, 6).await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, SERVICE_REMOVED, "start.dot");
    drop(guard);

    server
}

/// Remove A from config. A and C removed (C only dep of A). D stays (dep of B).
/// proxy1→B→D survives.
#[tokio::test]
async fn service_removed_remove_A() {
    let server = service_removed_setup().await;
    let new_config = load_config(SERVICE_REMOVED, "remove_A.toml").await;

    let mut guard = server.services().write().await;
    apply_config_update(&mut guard, new_config, server.orchestrator()).await;
    assert_graphviz(&guard, SERVICE_REMOVED, "after_remove_A.dot");
    drop(guard);

    // A→C, C→D, proxy1→A, proxy2→A freed; B→D, proxy1→B survive = 2 IDs
    assert_net_ids_in_use(&server, 2).await;

    let guard = server.services().read().await;
    assert!(!stack_view(&guard).contains_key("A"));
    assert!(!stack_view(&guard).contains_key("C"));
    assert!(stack_view(&guard).contains_key("B"));
    assert!(stack_view(&guard).contains_key("D"));
}

/// Remove B from config. D stays (dep of A). A chains survive.
#[tokio::test]
async fn service_removed_remove_B() {
    let server = service_removed_setup().await;
    let new_config = load_config(SERVICE_REMOVED, "remove_B.toml").await;

    let mut guard = server.services().write().await;
    apply_config_update(&mut guard, new_config, server.orchestrator()).await;
    assert_graphviz(&guard, SERVICE_REMOVED, "after_remove_B.dot");
    drop(guard);

    // B→D, proxy1→B freed; A→C, C→D, proxy1→A, proxy2→A survive = 4 IDs
    assert_net_ids_in_use(&server, 4).await;

    let guard = server.services().read().await;
    assert!(!stack_view(&guard).contains_key("B"));
    assert!(stack_view(&guard).contains_key("A"));
    assert!(stack_view(&guard).contains_key("C"));
    assert!(stack_view(&guard).contains_key("D"));
}

// ===========================================================================
// dep_changed: A→B→C, D→C (C shared). proxy1→A+D, proxy2→A
// ===========================================================================

const DEP_CHANGED: &str = "dep_changed";

async fn dep_changed_setup() -> NullnetGrpcImpl {
    let services = load_fixture(DEP_CHANGED).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    let ip_map = HashMap::from([
        ("A", ip(1, 1, 1, 1)),
        ("B", ip(2, 2, 2, 2)),
        ("C", ip(3, 3, 3, 3)),
        ("D", ip(4, 4, 4, 4)),
    ]);
    let proxy1 = ip(5, 5, 5, 5);
    let proxy2 = ip(6, 6, 6, 6);
    register_services(&server, &ip_map, 8080).await;
    server.orchestrator().register_fake_client(proxy1).await;
    server.orchestrator().register_fake_client(proxy2).await;

    setup_proxy_chain(&server, "A", proxy1, "10.0.0.1").await;
    setup_proxy_chain(&server, "D", proxy1, "10.0.0.1").await;
    setup_proxy_chain(&server, "A", proxy2, "10.0.0.2").await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, DEP_CHANGED, "start.dot");
    drop(guard);

    server
}

/// Add E to A's deps: [B,C] → [B,C,E]. A's chain cleaned up (dep change).
/// E added as unregistered. D→C survives.
#[tokio::test]
async fn dep_changed_add_E_to_A() {
    let server = dep_changed_setup().await;
    let new_config = load_config(DEP_CHANGED, "add_E_to_A.toml").await;

    let mut guard = server.services().write().await;
    apply_config_update(&mut guard, new_config, server.orchestrator()).await;
    assert_graphviz(&guard, DEP_CHANGED, "after_add_E_to_A.dot");

    assert_eq!(
        stack_view(&guard)["A"].proxy_deps(),
        vec!["B".to_string(), "C".to_string(), "E".to_string()]
    );
    assert!(stack_view(&guard).contains_key("E"));
    assert!(matches!(
        stack_view(&guard)["E"],
        ServiceInfo::Unregistered(_)
    ));
}

/// Drop C from A's deps: [B,C] → [B]. A's chain cleaned up but A stays registered.
/// D→C survives.
#[tokio::test]
async fn dep_changed_drop_C_from_A() {
    let server = dep_changed_setup().await;
    let new_config = load_config(DEP_CHANGED, "drop_C_from_A.toml").await;

    let mut guard = server.services().write().await;
    apply_config_update(&mut guard, new_config, server.orchestrator()).await;
    assert_graphviz(&guard, DEP_CHANGED, "after_drop_C_from_A.dot");

    assert!(stack_view(&guard).contains_key("A"));
    assert_eq!(stack_view(&guard)["A"].proxy_deps(), vec!["B".to_string()]);
    assert!(stack_view(&guard).contains_key("C"));
}

/// Drop all deps from D: [C] → []. D's chain cleaned up but D stays registered.
/// A chains survive.
#[tokio::test]
async fn dep_changed_drop_all_from_D() {
    let server = dep_changed_setup().await;
    let new_config = load_config(DEP_CHANGED, "drop_all_from_D.toml").await;

    let mut guard = server.services().write().await;
    apply_config_update(&mut guard, new_config, server.orchestrator()).await;
    assert_graphviz(&guard, DEP_CHANGED, "after_drop_all_from_D.dot");

    assert!(stack_view(&guard)["D"].proxy_deps().is_empty());
    assert!(stack_view(&guard).contains_key("C"));
}

/// Swap C for E in A's deps: [B,C] → [B,E]. A's chain cleaned up.
/// E added as unregistered. D→C survives.
#[tokio::test]
async fn dep_changed_swap_C_for_E() {
    let server = dep_changed_setup().await;
    let new_config = load_config(DEP_CHANGED, "swap_C_for_E.toml").await;

    let mut guard = server.services().write().await;
    apply_config_update(&mut guard, new_config, server.orchestrator()).await;
    assert_graphviz(&guard, DEP_CHANGED, "after_swap_C_for_E.dot");

    assert_eq!(
        stack_view(&guard)["A"].proxy_deps(),
        vec!["B".to_string(), "E".to_string()]
    );
    assert!(stack_view(&guard).contains_key("E"));
    assert!(matches!(
        stack_view(&guard)["E"],
        ServiceInfo::Unregistered(_)
    ));
}

// ===========================================================================
// reachability_changed: A→B→C, D→E. proxy1→A+D, proxy2→B.
// B is both proxy-reachable and a dep of A.
// ===========================================================================

const REACHABILITY_CHANGED: &str = "reachability_changed";

async fn reachability_changed_setup() -> NullnetGrpcImpl {
    let services = load_fixture(REACHABILITY_CHANGED).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    let ip_map = HashMap::from([
        ("A", ip(1, 1, 1, 1)),
        ("B", ip(2, 2, 2, 2)),
        ("C", ip(3, 3, 3, 3)),
        ("D", ip(4, 4, 4, 4)),
        ("E", ip(5, 5, 5, 5)),
    ]);
    let proxy1 = ip(6, 6, 6, 6);
    let proxy2 = ip(7, 7, 7, 7);
    register_services(&server, &ip_map, 8080).await;
    server.orchestrator().register_fake_client(proxy1).await;
    server.orchestrator().register_fake_client(proxy2).await;

    setup_proxy_chain(&server, "A", proxy1, "10.0.0.1").await;
    setup_proxy_chain(&server, "D", proxy1, "10.0.0.1").await;
    setup_proxy_chain(&server, "B", proxy2, "10.0.0.2").await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, REACHABILITY_CHANGED, "start.dot");
    drop(guard);

    server
}

/// B becomes unreachable (loses its [[services]] entry). B's own proxy chain
/// (proxy2→B) is torn down, but A's chain survives because B's deps are
/// correctly inferred from A's dependency list. D→E also survives.
#[tokio::test]
async fn reachability_changed_unreachable_B() {
    let server = reachability_changed_setup().await;
    let new_config = load_config(REACHABILITY_CHANGED, "unreachable_B.toml").await;

    let mut guard = server.services().write().await;
    apply_config_update(&mut guard, new_config, server.orchestrator()).await;
    assert_graphviz(&guard, REACHABILITY_CHANGED, "after_unreachable_B.dot");

    assert!(stack_view(&guard).contains_key("B"));
    assert!(stack_view(&guard)["B"].timeout().is_none());
}

/// D removed from [[services]] and no other service depends on it, so D and E
/// are fully removed from the map. proxy1→D and D→E torn down.
/// A and B chains survive.
#[tokio::test]
async fn reachability_changed_unreachable_D() {
    let server = reachability_changed_setup().await;
    let new_config = load_config(REACHABILITY_CHANGED, "unreachable_D.toml").await;

    let mut guard = server.services().write().await;
    apply_config_update(&mut guard, new_config, server.orchestrator()).await;
    assert_graphviz(&guard, REACHABILITY_CHANGED, "after_unreachable_D.dot");

    assert!(!stack_view(&guard).contains_key("D"));
    assert!(!stack_view(&guard).contains_key("E"));
}

// ===========================================================================
// service_unregistered: A→C→D, B→D (D shared). A+B co-located at 1.1.1.1.
// proxy1→A+B, proxy2→A
// ===========================================================================

const SERVICE_UNREGISTERED: &str = "service_unregistered";

async fn service_unregistered_setup() -> NullnetGrpcImpl {
    let services = load_fixture(SERVICE_UNREGISTERED).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    // A and B co-located at 1.1.1.1
    let ip_map = HashMap::from([
        ("A", ip(1, 1, 1, 1)),
        ("B", ip(1, 1, 1, 1)),
        ("C", ip(2, 2, 2, 2)),
        ("D", ip(3, 3, 3, 3)),
    ]);
    let proxy1 = ip(5, 5, 5, 5);
    let proxy2 = ip(6, 6, 6, 6);
    register_services(&server, &ip_map, 8080).await;
    server.orchestrator().register_fake_client(proxy1).await;
    server.orchestrator().register_fake_client(proxy2).await;

    setup_proxy_chain(&server, "A", proxy1, "10.0.0.1").await;
    setup_proxy_chain(&server, "B", proxy1, "10.0.0.1").await;
    setup_proxy_chain(&server, "A", proxy2, "10.0.0.2").await;

    assert_net_ids_in_use(&server, 6).await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, SERVICE_UNREGISTERED, "start.dot");
    drop(guard);

    server
}

/// Node 1.1.1.1 re-registers with only B (drops A selectively).
/// A's chains cleaned up. B→D survives.
#[tokio::test]
async fn service_unregistered_drop_A() {
    let server = service_unregistered_setup().await;

    server
        .apply_services_list(ip(1, 1, 1, 1), &[("B".into(), 8080, None)])
        .await
        .expect("apply_services_list failed");

    let guard = server.services().read().await;
    assert_graphviz(&guard, SERVICE_UNREGISTERED, "after_drop_A.dot");

    assert!(matches!(
        stack_view(&guard)["A"],
        ServiceInfo::Unregistered(_)
    ));
    assert!(matches!(
        stack_view(&guard)["B"],
        ServiceInfo::Registered(_)
    ));
}

/// Node 1.1.1.1 re-registers with only A (drops B selectively).
/// B's chain cleaned up. A chains survive.
#[tokio::test]
async fn service_unregistered_drop_B() {
    let server = service_unregistered_setup().await;

    server
        .apply_services_list(ip(1, 1, 1, 1), &[("A".into(), 8080, None)])
        .await
        .expect("apply_services_list failed");

    let guard = server.services().read().await;
    assert_graphviz(&guard, SERVICE_UNREGISTERED, "after_drop_B.dot");

    assert!(matches!(
        stack_view(&guard)["A"],
        ServiceInfo::Registered(_)
    ));
    assert!(matches!(
        stack_view(&guard)["B"],
        ServiceInfo::Unregistered(_)
    ));
}

/// Leaf dep host 2.2.2.2 re-registers with empty list (C unregistered).
/// Cascades to A (depends on C). B→D survives (B doesn't depend on C).
#[tokio::test]
async fn service_unregistered_drop_C() {
    let server = service_unregistered_setup().await;

    server
        .apply_services_list(ip(2, 2, 2, 2), &[])
        .await
        .expect("apply_services_list failed");

    let guard = server.services().read().await;
    assert_graphviz(&guard, SERVICE_UNREGISTERED, "after_drop_C.dot");

    assert!(matches!(
        stack_view(&guard)["C"],
        ServiceInfo::Unregistered(_)
    ));
    assert!(matches!(
        stack_view(&guard)["B"],
        ServiceInfo::Registered(_)
    ));
}

/// Shared dep host 3.3.3.3 re-registers with empty list (D unregistered).
/// Cascades to A (deps on D) and B (deps on D). All chains torn down.
/// All 6 NET IDs freed.
#[tokio::test]
async fn service_unregistered_drop_D() {
    let server = service_unregistered_setup().await;

    server
        .apply_services_list(ip(3, 3, 3, 3), &[])
        .await
        .expect("apply_services_list failed");

    let guard = server.services().read().await;
    assert_graphviz(&guard, SERVICE_UNREGISTERED, "after_drop_D.dot");
    drop(guard);

    // all 6 IDs freed
    assert_net_ids_in_use(&server, 0).await;

    let guard = server.services().read().await;
    assert!(matches!(
        stack_view(&guard)["D"],
        ServiceInfo::Unregistered(_)
    ));
}

// ===========================================================================
// node_disconnected: same topology as service_unregistered (A+B co-located).
// Contrasts: disconnect kills ALL services at that IP.
// ===========================================================================

const NODE_DISCONNECTED: &str = "node_disconnected";

async fn node_disconnected_setup() -> NullnetGrpcImpl {
    let services = load_fixture(NODE_DISCONNECTED).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    // A and B co-located at 1.1.1.1
    let ip_map = HashMap::from([
        ("A", ip(1, 1, 1, 1)),
        ("B", ip(1, 1, 1, 1)),
        ("C", ip(2, 2, 2, 2)),
        ("D", ip(3, 3, 3, 3)),
    ]);
    let proxy1 = ip(5, 5, 5, 5);
    let proxy2 = ip(6, 6, 6, 6);
    register_services(&server, &ip_map, 8080).await;
    server.orchestrator().register_fake_client(proxy1).await;
    server.orchestrator().register_fake_client(proxy2).await;

    setup_proxy_chain(&server, "A", proxy1, "10.0.0.1").await;
    setup_proxy_chain(&server, "B", proxy1, "10.0.0.1").await;
    setup_proxy_chain(&server, "A", proxy2, "10.0.0.2").await;

    assert_net_ids_in_use(&server, 6).await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, NODE_DISCONNECTED, "start.dot");
    drop(guard);

    server
}

/// A+B host (1.1.1.1) disconnects. BOTH A and B cleaned up (unlike
/// service_unregistered which can drop selectively).
/// All 6 NET IDs freed.
#[tokio::test]
async fn node_disconnected_A_B() {
    let server = node_disconnected_setup().await;

    server
        .orchestrator()
        .handle_node_disconnect(ip(1, 1, 1, 1), server.services())
        .await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, NODE_DISCONNECTED, "after_disconnect_A_B.dot");
    drop(guard);

    assert_net_ids_in_use(&server, 0).await;

    let guard = server.services().read().await;
    assert!(matches!(
        stack_view(&guard)["A"],
        ServiceInfo::Unregistered(_)
    ));
    assert!(matches!(
        stack_view(&guard)["B"],
        ServiceInfo::Unregistered(_)
    ));
}

/// C host (2.2.2.2) disconnects. C cleaned up, cascades to A (depends on C).
/// B→D survives (B doesn't depend on C).
#[tokio::test]
async fn node_disconnected_C() {
    let server = node_disconnected_setup().await;

    server
        .orchestrator()
        .handle_node_disconnect(ip(2, 2, 2, 2), server.services())
        .await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, NODE_DISCONNECTED, "after_disconnect_C.dot");

    assert!(matches!(
        stack_view(&guard)["C"],
        ServiceInfo::Unregistered(_)
    ));
    assert!(matches!(
        stack_view(&guard)["B"],
        ServiceInfo::Registered(_)
    ));
}

/// D host (3.3.3.3) disconnects. D cleaned up, cascades to A and B (both depend on D).
#[tokio::test]
async fn node_disconnected_D() {
    let server = node_disconnected_setup().await;

    server
        .orchestrator()
        .handle_node_disconnect(ip(3, 3, 3, 3), server.services())
        .await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, NODE_DISCONNECTED, "after_disconnect_D.dot");

    assert!(matches!(
        stack_view(&guard)["D"],
        ServiceInfo::Unregistered(_)
    ));
}

/// Proxy1 (5.5.5.5) disconnects. proxy1→A and proxy1→B chains torn down.
/// proxy2→A survives, so A→C→D edges survive. All services stay registered.
#[tokio::test]
async fn node_disconnected_proxy1() {
    let server = node_disconnected_setup().await;

    server
        .orchestrator()
        .handle_node_disconnect(ip(5, 5, 5, 5), server.services())
        .await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, NODE_DISCONNECTED, "after_disconnect_proxy1.dot");
    drop(guard);

    // proxy1→A, proxy1→B, B→D freed; A→C, C→D, proxy2→A survive = 3 IDs
    assert_net_ids_in_use(&server, 3).await;

    let guard = server.services().read().await;
    assert!(matches!(
        stack_view(&guard)["A"],
        ServiceInfo::Registered(_)
    ));
    assert!(matches!(
        stack_view(&guard)["B"],
        ServiceInfo::Registered(_)
    ));
    assert!(matches!(
        stack_view(&guard)["C"],
        ServiceInfo::Registered(_)
    ));
    assert!(matches!(
        stack_view(&guard)["D"],
        ServiceInfo::Registered(_)
    ));
}

// ===========================================================================
// proxy_timeout: A→C→D, B→D (D shared). proxy1→A+B, proxy2→A.
// A has timeout=1, B has timeout=2.
// ===========================================================================

const PROXY_TIMEOUT: &str = "proxy_timeout";

async fn proxy_timeout_setup() -> NullnetGrpcImpl {
    let services = load_fixture(PROXY_TIMEOUT).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    let ip_map = HashMap::from([
        ("A", ip(1, 1, 1, 1)),
        ("B", ip(2, 2, 2, 2)),
        ("C", ip(3, 3, 3, 3)),
        ("D", ip(4, 4, 4, 4)),
    ]);
    let proxy1 = ip(5, 5, 5, 5);
    let proxy2 = ip(6, 6, 6, 6);
    register_services(&server, &ip_map, 8080).await;
    server.orchestrator().register_fake_client(proxy1).await;
    server.orchestrator().register_fake_client(proxy2).await;

    setup_proxy_chain(&server, "A", proxy1, "10.0.0.1").await;
    setup_proxy_chain(&server, "B", proxy1, "10.0.0.1").await;
    setup_proxy_chain(&server, "A", proxy2, "10.0.0.2").await;

    assert_net_ids_in_use(&server, 6).await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, PROXY_TIMEOUT, "start.dot");
    drop(guard);

    server
}

/// After A's timeout (1s), both proxy clients on A expire. B's proxy client
/// survives (timeout=2). A→C→D edges removed (no more proxy clients on A).
/// B→D edge survives.
#[tokio::test]
async fn proxy_timeout_A() {
    let server = proxy_timeout_setup().await;

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    let mut guard = server.services().write().await;
    apply_timeouts(stack_view_mut(&mut guard), server.orchestrator()).await;
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

/// After B's timeout (2s), B's proxy client also expires.
/// All proxy chains gone; all services still registered.
#[tokio::test]
async fn proxy_timeout_A_then_B() {
    let server = proxy_timeout_setup().await;

    // A expires after 1s
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let mut guard = server.services().write().await;
    apply_timeouts(stack_view_mut(&mut guard), server.orchestrator()).await;
    assert_graphviz(&guard, PROXY_TIMEOUT, "after_timeout_A.dot");
    drop(guard);

    // B expires after 2s total
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let mut guard = server.services().write().await;
    apply_timeouts(stack_view_mut(&mut guard), server.orchestrator()).await;
    assert_graphviz(&guard, PROXY_TIMEOUT, "after_timeout_A_then_B.dot");

    // all services still registered, but no proxy clients left
    for (_, si) in stack_view(&guard).iter() {
        if let ServiceInfo::Registered(reg) = si {
            assert!(
                !reg.has_clients(),
                "expected no proxy clients after both timeouts"
            );
        }
    }
    drop(guard);

    // all 6 IDs freed
    assert_net_ids_in_use(&server, 0).await;
}

/// After 2s+ both A and B expire simultaneously in a single apply.
/// All 6 NET IDs freed.
#[tokio::test]
async fn proxy_timeout_all_at_once() {
    let server = proxy_timeout_setup().await;

    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;

    let mut guard = server.services().write().await;
    apply_timeouts(stack_view_mut(&mut guard), server.orchestrator()).await;
    assert_graphviz(&guard, PROXY_TIMEOUT, "after_timeout_all.dot");

    for (_, si) in stack_view(&guard).iter() {
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

/// Config update tightens B's timeout from 2→1. After 1.5s, B's clients
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
// multi_replica: A→B, C→B. B has 3 replicas across 2 IPs:
//   - 2.2.2.2 container "b1"
//   - 2.2.2.2 container "b2"  (Docker Swarm: two containers, same host)
//   - 4.4.4.4 (no container)
// Tests round-robin, sticky sessions, and partial replica removal.
// ===========================================================================

const MULTI_REPLICA: &str = "multi_replica";

/// Replicas:
///   A: "a1" and "a2" on 1.1.1.1 (Docker Swarm)
///   B: "b1" on 2.2.2.2, standalone on 4.4.4.4, "b2" on 2.2.2.2
///   C: standalone on 3.3.3.3
///   D: standalone on 6.6.6.6
async fn multi_replica_setup() -> NullnetGrpcImpl {
    let services = load_fixture(MULTI_REPLICA).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    // A: 2 replicas on same IP (Docker Swarm containers "a1", "a2")
    {
        let mut services = server.services().write().await;
        stack_view_mut(&mut services)
            .get_mut("A")
            .unwrap()
            .add_replica(ip(1, 1, 1, 1), 8080, Some("a1".into()));
        stack_view_mut(&mut services)
            .get_mut("A")
            .unwrap()
            .add_replica(ip(1, 1, 1, 1), 8080, Some("a2".into()));
    }
    server
        .orchestrator()
        .register_fake_client(ip(1, 1, 1, 1))
        .await;

    // B: 3 replicas — insertion order matters for least-clients tie-breaking
    {
        let mut services = server.services().write().await;
        stack_view_mut(&mut services)
            .get_mut("B")
            .unwrap()
            .add_replica(ip(2, 2, 2, 2), 8080, Some("b1".into()));
        stack_view_mut(&mut services)
            .get_mut("B")
            .unwrap()
            .add_replica(ip(4, 4, 4, 4), 8080, None);
        stack_view_mut(&mut services)
            .get_mut("B")
            .unwrap()
            .add_replica(ip(2, 2, 2, 2), 8080, Some("b2".into()));
    }
    server
        .orchestrator()
        .register_fake_client(ip(2, 2, 2, 2))
        .await;
    server
        .orchestrator()
        .register_fake_client(ip(4, 4, 4, 4))
        .await;

    // C: 1 replica on 3.3.3.3
    {
        let mut services = server.services().write().await;
        stack_view_mut(&mut services)
            .get_mut("C")
            .unwrap()
            .add_replica(ip(3, 3, 3, 3), 8080, None);
    }
    server
        .orchestrator()
        .register_fake_client(ip(3, 3, 3, 3))
        .await;

    // D: 1 replica on 6.6.6.6
    {
        let mut services = server.services().write().await;
        stack_view_mut(&mut services)
            .get_mut("D")
            .unwrap()
            .add_replica(ip(6, 6, 6, 6), 8080, None);
    }
    server
        .orchestrator()
        .register_fake_client(ip(6, 6, 6, 6))
        .await;

    server
}

/// B has 3 replicas across 2 IPs. Verify all are present.
#[tokio::test]
async fn multi_replica_register() {
    let server = multi_replica_setup().await;
    let guard = server.services().read().await;

    let ServiceInfo::Registered(reg) = &stack_view(&guard)["B"] else {
        panic!("B should be registered");
    };
    assert_eq!(reg.replicas().len(), 3);
    // 2 replicas on 2.2.2.2 (containers "b1" and "b2"), 1 on 4.4.4.4
    assert!(reg.has_replica_on_ip(ip(2, 2, 2, 2)));
    assert!(reg.has_replica_on_ip(ip(4, 4, 4, 4)));
    let on_2: Vec<_> = reg
        .replicas()
        .iter()
        .filter(|r| r.ip() == ip(2, 2, 2, 2))
        .collect();
    assert_eq!(
        on_2.len(),
        2,
        "2.2.2.2 should have 2 replicas (Docker Swarm)"
    );
    let containers: HashSet<_> = on_2.iter().filter_map(|r| r.docker_container()).collect();
    assert!(containers.contains("b1"));
    assert!(containers.contains("b2"));
}

/// Least-clients: proxy requests and dependency chains pick the replica
/// with the fewest active clients.  After two proxy chains (A and C both
/// depend on B), B's 3 replicas should spread: the second chain picks a
/// different replica than the first.
#[tokio::test]
async fn multi_replica_least_clients() {
    let server = multi_replica_setup().await;
    let proxy1 = ip(5, 5, 5, 5);
    server.orchestrator().register_fake_client(proxy1).await;

    // First chain: proxy1 -> A -> B (picks least-loaded B replica, all empty)
    setup_proxy_chain(&server, "A", proxy1, "10.0.0.1").await;

    {
        let guard = server.services().read().await;
        let ServiceInfo::Registered(reg_b) = &stack_view(&guard)["B"] else {
            panic!("B should be registered");
        };
        assert_eq!(
            reg_b.client_count(),
            1,
            "B should have 1 client after first chain"
        );
    }

    // Second chain: proxy1 -> C -> B (picks a *different* B replica since the
    // first now has 1 client while two others have 0)
    setup_proxy_chain(&server, "C", proxy1, "10.0.0.2").await;

    let guard = server.services().read().await;

    let ServiceInfo::Registered(reg_b) = &stack_view(&guard)["B"] else {
        panic!("B should be registered");
    };

    // B should now have 2 clients spread across replicas
    assert_eq!(reg_b.client_count(), 2, "B should have 2 clients total");

    // No single replica should hold both
    for replica in reg_b.replicas() {
        assert!(
            replica.clients().len() <= 1,
            "each B replica should have at most 1 client, got {}",
            replica.clients().len()
        );
    }
}

/// Sticky session: same proxy client reconnects to the same replica.
#[tokio::test]
async fn multi_replica_sticky_session() {
    let server = multi_replica_setup().await;
    let proxy1 = ip(5, 5, 5, 5);
    server.orchestrator().register_fake_client(proxy1).await;

    // First request from client 10.0.0.1
    setup_proxy_chain(&server, "A", proxy1, "10.0.0.1").await;

    // Record which upstream the client got
    let first_upstream = {
        let guard = server.services().read().await;
        let ServiceInfo::Registered(reg) = &stack_view(&guard)["A"] else {
            panic!("A should be registered");
        };
        let client = crate::services::clients::Client::new("10.0.0.1".to_string(), Some(proxy1));
        reg.is_client_setup(&client)
            .expect("client should be set up")
    };

    // Second lookup from same client — should be sticky (same upstream)
    let second_upstream = {
        let guard = server.services().read().await;
        let ServiceInfo::Registered(reg) = &stack_view(&guard)["A"] else {
            panic!("A should be registered");
        };
        let client = crate::services::clients::Client::new("10.0.0.1".to_string(), Some(proxy1));
        reg.is_client_setup(&client)
            .expect("client should still be set up")
    };

    assert_eq!(
        first_upstream, second_upstream,
        "sticky session should return same upstream"
    );
}

/// Partial replica removal: disconnect 2.2.2.2 removes two of B's replicas
/// ("b1" and "b2"), but B stays registered via the replica at 4.4.4.4.
///
/// With least-clients distribution:
///   A→B lands on "b1" (2.2.2.2)  — affected by disconnect
///   C→B lands on 4.4.4.4         — NOT affected, chain survives
#[tokio::test]
async fn multi_replica_partial_disconnect() {
    let server = multi_replica_setup().await;
    let proxy1 = ip(5, 5, 5, 5);
    server.orchestrator().register_fake_client(proxy1).await;

    setup_proxy_chain(&server, "A", proxy1, "10.0.0.1").await;
    setup_proxy_chain(&server, "C", proxy1, "10.0.0.2").await;

    // A→B, proxy→A, C→B, proxy→C = 4 NET IDs
    assert_net_ids_in_use(&server, 4).await;

    // Disconnect 2.2.2.2 — removes "b1" and "b2" replicas.
    // Only A→B (on "b1") is affected; C→B (on 4.4.4.4) survives.
    server
        .orchestrator()
        .handle_node_disconnect(ip(2, 2, 2, 2), server.services())
        .await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, MULTI_REPLICA, "after_partial_disconnect.dot");

    // B should still be registered (has replica at 4.4.4.4)
    assert!(
        matches!(stack_view(&guard)["B"], ServiceInfo::Registered(_)),
        "B should still be registered with remaining replica"
    );
    if let ServiceInfo::Registered(reg) = &stack_view(&guard)["B"] {
        assert_eq!(reg.replicas().len(), 1, "B should have 1 replica left");
        assert!(reg.has_replica_on_ip(ip(4, 4, 4, 4)));
        assert!(!reg.has_replica_on_ip(ip(2, 2, 2, 2)));
        // C→B on 4.4.4.4 survived — B still has 1 client
        assert_eq!(reg.client_count(), 1, "C→B chain should survive");
    }

    // A's chain was torn down (A→B was on removed replica)
    if let ServiceInfo::Registered(reg) = &stack_view(&guard)["A"] {
        assert!(!reg.has_clients(), "A should have no proxy clients");
    }

    // C's chain survived (C→B was on 4.4.4.4)
    if let ServiceInfo::Registered(reg) = &stack_view(&guard)["C"] {
        assert_eq!(
            reg.client_count(),
            1,
            "C should still have its proxy client"
        );
    }

    // A→B and proxy→A freed; C→B and proxy→C survive = 2 NET IDs
    drop(guard);
    assert_net_ids_in_use(&server, 2).await;
}

/// Full replica removal: disconnect 2.2.2.2 (removes "b1" + "b2"), then
/// disconnect 4.4.4.4 (removes last replica). B becomes unregistered and
/// cascades to A and C.
#[tokio::test]
async fn multi_replica_full_disconnect() {
    let server = multi_replica_setup().await;
    let proxy1 = ip(5, 5, 5, 5);
    server.orchestrator().register_fake_client(proxy1).await;

    setup_proxy_chain(&server, "A", proxy1, "10.0.0.1").await;
    setup_proxy_chain(&server, "C", proxy1, "10.0.0.2").await;

    // Disconnect 2.2.2.2 (partial) — removes 2 of 3 replicas, cascades chains through B
    server
        .orchestrator()
        .handle_node_disconnect(ip(2, 2, 2, 2), server.services())
        .await;

    {
        let guard = server.services().read().await;
        assert_graphviz(&guard, MULTI_REPLICA, "after_partial_disconnect.dot");
        // B should still be registered (has replica at 4.4.4.4)
        assert!(matches!(
            stack_view(&guard)["B"],
            ServiceInfo::Registered(_)
        ));
        if let ServiceInfo::Registered(reg) = &stack_view(&guard)["B"] {
            assert_eq!(reg.replicas().len(), 1);
        }
    }

    // Disconnect 4.4.4.4 — last replica, full teardown
    server
        .orchestrator()
        .handle_node_disconnect(ip(4, 4, 4, 4), server.services())
        .await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, MULTI_REPLICA, "after_full_disconnect.dot");

    // B should be unregistered (no replicas)
    assert!(
        matches!(stack_view(&guard)["B"], ServiceInfo::Unregistered(_)),
        "B should be unregistered with no replicas"
    );

    // All NET IDs should be freed
    drop(guard);
    assert_net_ids_in_use(&server, 0).await;
}

/// ServicesList from two hosts: host 2.2.2.2 sends two containers ("b1", "b2"),
/// host 4.4.4.4 sends one standalone replica. Then host 2.2.2.2 re-registers
/// with empty list — both its replicas are removed, but 4.4.4.4's stays.
#[tokio::test]
async fn multi_replica_via_services_list() {
    let services = load_fixture(MULTI_REPLICA).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    // Host 2.2.2.2 registers B in two containers
    server
        .orchestrator()
        .register_fake_client(ip(2, 2, 2, 2))
        .await;
    server
        .apply_services_list(
            ip(2, 2, 2, 2),
            &[
                ("B".into(), 8080, Some("b1".into())),
                ("B".into(), 8080, Some("b2".into())),
            ],
        )
        .await
        .expect("apply_services_list failed");

    // Host 4.4.4.4 registers B standalone
    server
        .orchestrator()
        .register_fake_client(ip(4, 4, 4, 4))
        .await;
    server
        .apply_services_list(ip(4, 4, 4, 4), &[("B".into(), 9090, None)])
        .await
        .expect("apply_services_list failed");

    {
        let guard = server.services().read().await;
        let ServiceInfo::Registered(reg) = &stack_view(&guard)["B"] else {
            panic!("B should be registered");
        };
        assert_eq!(reg.replicas().len(), 3);
        let on_2: Vec<_> = reg
            .replicas()
            .iter()
            .filter(|r| r.ip() == ip(2, 2, 2, 2))
            .collect();
        assert_eq!(on_2.len(), 2, "2.2.2.2 should have 2 replicas");
        assert!(reg.has_replica_on_ip(ip(4, 4, 4, 4)));
    }

    // Host 2.2.2.2 re-registers WITHOUT B → both its replicas removed
    server
        .apply_services_list(ip(2, 2, 2, 2), &[])
        .await
        .expect("apply_services_list failed");

    let guard = server.services().read().await;
    let ServiceInfo::Registered(reg) = &stack_view(&guard)["B"] else {
        panic!("B should still be registered");
    };
    assert_eq!(
        reg.replicas().len(),
        1,
        "only 4.4.4.4 replica should remain"
    );
    assert!(reg.has_replica_on_ip(ip(4, 4, 4, 4)));
    assert!(!reg.has_replica_on_ip(ip(2, 2, 2, 2)));
}

/// Docker Swarm: host 2.2.2.2 runs containers "b1" and "b2". Container "b1"
/// dies, so the host re-registers with only "b2". Only "b1" is removed;
/// "b2" and the standalone 4.4.4.4 replica survive.
#[tokio::test]
async fn multi_replica_single_container_removed() {
    let services = load_fixture(MULTI_REPLICA).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    // Host 2.2.2.2 registers B in two containers
    server
        .orchestrator()
        .register_fake_client(ip(2, 2, 2, 2))
        .await;
    server
        .apply_services_list(
            ip(2, 2, 2, 2),
            &[
                ("B".into(), 8080, Some("b1".into())),
                ("B".into(), 8080, Some("b2".into())),
            ],
        )
        .await
        .expect("apply_services_list failed");

    // Host 4.4.4.4 registers B standalone
    server
        .orchestrator()
        .register_fake_client(ip(4, 4, 4, 4))
        .await;
    server
        .apply_services_list(ip(4, 4, 4, 4), &[("B".into(), 9090, None)])
        .await
        .expect("apply_services_list failed");

    {
        let guard = server.services().read().await;
        let ServiceInfo::Registered(reg) = &stack_view(&guard)["B"] else {
            panic!("B should be registered");
        };
        assert_eq!(reg.replicas().len(), 3);
    }

    // Container "b1" dies — host re-registers with only "b2"
    server
        .apply_services_list(ip(2, 2, 2, 2), &[("B".into(), 8080, Some("b2".into()))])
        .await
        .expect("apply_services_list failed");

    let guard = server.services().read().await;
    let ServiceInfo::Registered(reg) = &stack_view(&guard)["B"] else {
        panic!("B should still be registered");
    };

    // "b1" removed, "b2" and 4.4.4.4 survive → 2 replicas left
    assert_eq!(reg.replicas().len(), 2, "should have 2 replicas left");
    assert!(
        reg.has_replica_on_ip(ip(2, 2, 2, 2)),
        "2.2.2.2 should still have a replica"
    );
    assert!(
        reg.has_replica_on_ip(ip(4, 4, 4, 4)),
        "4.4.4.4 should still have a replica"
    );

    // The surviving 2.2.2.2 replica should be "b2"
    let on_2: Vec<_> = reg
        .replicas()
        .iter()
        .filter(|r| r.ip() == ip(2, 2, 2, 2))
        .collect();
    assert_eq!(on_2.len(), 1, "only one replica on 2.2.2.2");
    assert_eq!(on_2[0].docker_container(), Some("b2"));
}

/// Helper: set up all 5 proxy chains for the multi-replica topology.
///
/// Chain order is chosen so A's two replicas land on different B replicas:
///   proxy1→A(a1)→B(b1), proxy1→C→B(4.4.4.4), proxy2→A(a2)→B(b2),
///   proxy1→D→B(b1), proxy2→C→B(4.4.4.4 reuse, add_chain)
///
/// B's load: b1 has A(a1)+D, 4.4.4.4 has C(chains=2), b2 has A(a2)
async fn setup_all_chains(server: &NullnetGrpcImpl) {
    let proxy1 = ip(5, 5, 5, 5);
    let proxy2 = ip(7, 7, 7, 7);
    server.orchestrator().register_fake_client(proxy1).await;
    server.orchestrator().register_fake_client(proxy2).await;

    // proxy1→A(a1)→B(b1):  all B replicas at 0, b1 wins tie
    setup_proxy_chain(server, "A", proxy1, "10.0.0.1").await;
    // proxy1→C→B(4.4.4.4): b1=1, 4.4.4.4=0, b2=0 → 4.4.4.4
    setup_proxy_chain(server, "C", proxy1, "10.0.0.2").await;
    // proxy2→A(a2)→B(b2):  b1=1, 4.4.4.4=1, b2=0 → b2
    setup_proxy_chain(server, "A", proxy2, "10.0.0.4").await;
    // proxy1→D→B(b1):      b1=1, 4.4.4.4=1, b2=1 → b1 (tie)
    setup_proxy_chain(server, "D", proxy1, "10.0.0.3").await;
    // proxy2→C→B(4.4.4.4): C already on 4.4.4.4 → add_chain
    setup_proxy_chain(server, "C", proxy2, "10.0.0.5").await;
}

/// Verify the comprehensive topology's initial state: replica counts,
/// client distribution, and mixed load across B's replicas.
///
/// Each A replica (a1, a2) creates a separate VXLAN to B since they are
/// distinct physical connections. B ends up with 4 client entries:
///   b1: A(a1) + D  — overloaded
///   4.4.4.4: C (chains=2, shared across both proxy chains)
///   b2: A(a2)
#[tokio::test]
async fn multi_replica_comprehensive_register() {
    let server = multi_replica_setup().await;
    setup_all_chains(&server).await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, MULTI_REPLICA, "start.dot");

    // A: 2 replicas, 2 proxy clients (one per replica)
    let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] else {
        panic!("A should be registered");
    };
    assert_eq!(reg_a.replicas().len(), 2);
    assert_eq!(reg_a.client_count(), 2, "A should have 2 proxy clients");

    // B: 3 replicas, 4 client entries.
    // A(a1) and A(a2) are distinct Clients (different source replicas),
    // each with their own VXLAN to b1.
    let ServiceInfo::Registered(reg_b) = &stack_view(&guard)["B"] else {
        panic!("B should be registered");
    };
    assert_eq!(reg_b.replicas().len(), 3);
    assert_eq!(
        reg_b.client_count(),
        4,
        "B should have 4 client entries: A(a1)+D on b1, C on 4.4.4.4, A(a2) on b2"
    );
    // b1 is overloaded (2 clients: A(a1) + D), b2 has A(a2), 4.4.4.4 has C
    let b1 = reg_b
        .replicas()
        .iter()
        .find(|r| r.ip() == ip(2, 2, 2, 2) && r.docker_container() == Some("b1"))
        .expect("b1 should exist");
    assert_eq!(b1.clients().len(), 2, "b1 should have 2 clients: A(a1) + D");
    let on_444 = reg_b
        .replicas()
        .iter()
        .find(|r| r.ip() == ip(4, 4, 4, 4))
        .expect("4.4.4.4 should exist");
    assert_eq!(
        on_444.clients().len(),
        1,
        "4.4.4.4 should have 1 client (C)"
    );
    let b2 = reg_b
        .replicas()
        .iter()
        .find(|r| r.ip() == ip(2, 2, 2, 2) && r.docker_container() == Some("b2"))
        .expect("b2 should exist");
    assert_eq!(b2.clients().len(), 1, "b2 should have 1 client: A(a2)");

    // C: 1 replica, 2 proxy clients (overloaded)
    let ServiceInfo::Registered(reg_c) = &stack_view(&guard)["C"] else {
        panic!("C should be registered");
    };
    assert_eq!(reg_c.replicas().len(), 1);
    assert_eq!(
        reg_c.client_count(),
        2,
        "C should have 2 proxy clients (overloaded)"
    );

    // D: 1 replica, 1 proxy client (minimal)
    let ServiceInfo::Registered(reg_d) = &stack_view(&guard)["D"] else {
        panic!("D should be registered");
    };
    assert_eq!(reg_d.replicas().len(), 1);
    assert_eq!(reg_d.client_count(), 1, "D should have 1 proxy client");

    // 9 NET IDs: A(a1)→B, A(a2)→B, C→B, D→B,
    //            proxy1→A, proxy2→A, proxy1→C, proxy2→C, proxy1→D
    drop(guard);
    assert_net_ids_in_use(&server, 9).await;
}

/// Same-IP container disconnect on dependency B: container "b1" on 2.2.2.2
/// dies while "b2" (same IP) survives.
///
/// b1 hosted A(a1)→B and D→B. Only chains through a1 and D are torn down.
/// proxy2→A(a2)→B(b2) survives because its dep chain goes through b2.
/// C→B on 4.4.4.4 also survives.
#[tokio::test]
async fn multi_replica_b_same_ip_container_disconnect() {
    let server = multi_replica_setup().await;
    setup_all_chains(&server).await;
    assert_net_ids_in_use(&server, 9).await;

    {
        let guard = server.services().read().await;
        assert_graphviz(&guard, MULTI_REPLICA, "start.dot");
    }

    // Container "b1" dies — host 2.2.2.2 re-registers with only "b2"
    server
        .apply_services_list(ip(2, 2, 2, 2), &[("B".into(), 8080, Some("b2".into()))])
        .await
        .expect("apply_services_list failed");

    let guard = server.services().read().await;
    assert_graphviz(&guard, MULTI_REPLICA, "after_b_same_ip_disconnect.dot");

    // B: 2 replicas left (b2 + 4.4.4.4).
    // A(a1)→B and D→B on b1 torn down. A(a2)→B on b2 and C→B on 4.4.4.4 survive.
    let ServiceInfo::Registered(reg_b) = &stack_view(&guard)["B"] else {
        panic!("B should still be registered");
    };
    assert_eq!(reg_b.replicas().len(), 2, "B should have 2 replicas left");
    assert!(reg_b.has_replica_on_ip(ip(2, 2, 2, 2)), "b2 should survive");
    assert!(
        reg_b.has_replica_on_ip(ip(4, 4, 4, 4)),
        "standalone should survive"
    );
    let on_2: Vec<_> = reg_b
        .replicas()
        .iter()
        .filter(|r| r.ip() == ip(2, 2, 2, 2))
        .collect();
    assert_eq!(on_2.len(), 1, "only b2 should remain on 2.2.2.2");
    assert_eq!(on_2[0].docker_container(), Some("b2"));
    assert_eq!(
        reg_b.client_count(),
        2,
        "A(a2)→B on b2 and C→B on 4.4.4.4 should survive"
    );

    // A: only proxy1→A(a1) torn down; proxy2→A(a2) survives (its chain goes through b2)
    if let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] {
        assert_eq!(
            reg_a.client_count(),
            1,
            "proxy2→A(a2) should survive (chain through b2)"
        );
    }

    // C: both proxies survive (C→B on 4.4.4.4 unaffected by b1 removal)
    if let ServiceInfo::Registered(reg_c) = &stack_view(&guard)["C"] {
        assert_eq!(
            reg_c.client_count(),
            2,
            "C should still have 2 proxy clients"
        );
    }

    // D: proxy torn down (D→B was on b1)
    if let ServiceInfo::Registered(reg_d) = &stack_view(&guard)["D"] {
        assert!(
            !reg_d.has_clients(),
            "D should have no clients after b1 removal"
        );
    }

    // Freed: A(a1)→B, D→B, proxy1→A, proxy1→D = 4
    // Remaining: A(a2)→B, C→B, proxy2→A, proxy1→C, proxy2→C = 5
    drop(guard);
    assert_net_ids_in_use(&server, 5).await;
}

/// Different-IP disconnect on dependency B: node 4.4.4.4 goes offline.
///
/// C→B was on 4.4.4.4 → teardown_chain("C") tears down C's chain.
/// A(a1)→B on b1, A(a2)→B on b2, and D→B on b1 all survive.
#[tokio::test]
async fn multi_replica_b_different_ip_disconnect() {
    let server = multi_replica_setup().await;
    setup_all_chains(&server).await;
    assert_net_ids_in_use(&server, 9).await;

    {
        let guard = server.services().read().await;
        assert_graphviz(&guard, MULTI_REPLICA, "start.dot");
    }

    // Disconnect 4.4.4.4 — removes B's standalone replica
    server
        .orchestrator()
        .handle_node_disconnect(ip(4, 4, 4, 4), server.services())
        .await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, MULTI_REPLICA, "after_b_different_ip_disconnect.dot");

    // B: 2 replicas left (b1 + b2 on 2.2.2.2).
    // C fully torn down (was on 4.4.4.4).
    // A(a1)+D on b1 and A(a2) on b2 survive.
    let ServiceInfo::Registered(reg_b) = &stack_view(&guard)["B"] else {
        panic!("B should still be registered");
    };
    assert_eq!(reg_b.replicas().len(), 2, "B should have 2 replicas left");
    assert!(reg_b.has_replica_on_ip(ip(2, 2, 2, 2)));
    assert!(!reg_b.has_replica_on_ip(ip(4, 4, 4, 4)));
    assert_eq!(
        reg_b.client_count(),
        3,
        "A(a1)+D on b1 and A(a2) on b2 should survive"
    );

    // A: both proxies survive (A→B edges on b1 and b2 unaffected)
    if let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] {
        assert_eq!(
            reg_a.client_count(),
            2,
            "A should still have 2 proxy clients"
        );
    }

    // C: all proxy chains torn down (C→B was on 4.4.4.4)
    if let ServiceInfo::Registered(reg_c) = &stack_view(&guard)["C"] {
        assert!(
            !reg_c.has_clients(),
            "C should have no clients after 4.4.4.4 removal"
        );
    }

    // D: proxy survives (D→B on b1 at 2.2.2.2)
    if let ServiceInfo::Registered(reg_d) = &stack_view(&guard)["D"] {
        assert_eq!(
            reg_d.client_count(),
            1,
            "D should still have its proxy client"
        );
    }

    // Freed: C→B, proxy1→C, proxy2→C = 3
    // Remaining: A(a1)→B, A(a2)→B, proxy1→A, proxy2→A, D→B, proxy1→D = 6
    drop(guard);
    assert_net_ids_in_use(&server, 6).await;
}

/// Same-IP container disconnect on first-step service A: container "a1"
/// on 1.1.1.1 dies while "a2" (same IP) survives.
///
/// Only the proxy client on a1 is torn down. The proxy on a2 survives,
/// and the underlying A→B dependency is unaffected.
#[tokio::test]
async fn multi_replica_first_step_container_disconnect() {
    let server = multi_replica_setup().await;
    setup_all_chains(&server).await;
    assert_net_ids_in_use(&server, 9).await;

    {
        let guard = server.services().read().await;
        assert_graphviz(&guard, MULTI_REPLICA, "start.dot");
    }

    // Container "a1" dies — host 1.1.1.1 re-registers with only "a2"
    server
        .apply_services_list(ip(1, 1, 1, 1), &[("A".into(), 8080, Some("a2".into()))])
        .await
        .expect("apply_services_list failed");

    let guard = server.services().read().await;
    assert_graphviz(&guard, MULTI_REPLICA, "after_first_step_disconnect.dot");

    // A: 1 replica left (a2), 1 proxy client survives (the one on a2)
    let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] else {
        panic!("A should still be registered");
    };
    assert_eq!(
        reg_a.replicas().len(),
        1,
        "A should have 1 replica left (a2)"
    );
    let surviving = &reg_a.replicas()[0];
    assert_eq!(surviving.docker_container(), Some("a2"));
    assert_eq!(reg_a.client_count(), 1, "only proxy on a2 should survive");

    // B: A(a1)→B(b1) also torn down (orphaned VXLAN). A(a2)→B(b1) survives.
    // 3 client entries remain: A(a2) on b1, C on 4.4.4.4, D on b2.
    let ServiceInfo::Registered(reg_b) = &stack_view(&guard)["B"] else {
        panic!("B should still be registered");
    };
    assert_eq!(reg_b.replicas().len(), 3);
    assert_eq!(
        reg_b.client_count(),
        3,
        "A(a1)→B should be torn down, 3 clients remain"
    );

    // C: unaffected
    if let ServiceInfo::Registered(reg_c) = &stack_view(&guard)["C"] {
        assert_eq!(
            reg_c.client_count(),
            2,
            "C should still have 2 proxy clients"
        );
    }

    // D: unaffected
    if let ServiceInfo::Registered(reg_d) = &stack_view(&guard)["D"] {
        assert_eq!(
            reg_d.client_count(),
            1,
            "D should still have its proxy client"
        );
    }

    // Freed: proxy1→A + A(a1)→B = 2; remaining = 7
    drop(guard);
    assert_net_ids_in_use(&server, 7).await;
}

// ===========================================================================
// max_networks: A→B, max_networks=1, timeout=1.
// Two proxy clients on the same proxy: second reuses the first's network.
// ===========================================================================

const MAX_NETWORKS: &str = "max_networks";

/// Full lifecycle:
///   1. First client creates network (2 net IDs: proxy→A, A→B)
///   2. Second client reuses (still 2 net IDs, but A has 2 proxy clients)
///   3. First client times out (net stays up, A has 1 proxy client)
///   4. Second client times out (net torn down, 0 net IDs)
#[tokio::test]
async fn max_networks_reuse_lifecycle() {
    let services = load_fixture(MAX_NETWORKS).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    let ip_map = HashMap::from([("A", ip(1, 1, 1, 1)), ("B", ip(2, 2, 2, 2))]);
    let proxy1 = ip(5, 5, 5, 5);
    register_services(&server, &ip_map, 8080).await;
    server.orchestrator().register_fake_client(proxy1).await;

    // 1. First proxy client — creates fresh networks
    setup_proxy_chain(&server, "A", proxy1, "10.0.0.1").await;
    assert_net_ids_in_use(&server, 2).await; // proxy→A + A→B

    {
        let guard = server.services().read().await;
        assert_graphviz(&guard, MAX_NETWORKS, "after_first_client.dot");
        let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] else {
            panic!("A should be registered");
        };
        assert_eq!(reg_a.client_count(), 1, "A should have 1 proxy client");
        let ServiceInfo::Registered(reg_b) = &stack_view(&guard)["B"] else {
            panic!("B should be registered");
        };
        assert_eq!(reg_b.client_count(), 1, "B should have 1 dep client (A→B)");
    }

    // 2. Second proxy client on same proxy — should reuse (max_networks=1)
    //    Delay so the two clients have different `latest` timestamps,
    //    allowing the first to time out independently.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    setup_proxy_chain(&server, "A", proxy1, "10.0.0.2").await;
    assert_net_ids_in_use(&server, 2).await; // no new net IDs allocated

    {
        let guard = server.services().read().await;
        assert_graphviz(&guard, MAX_NETWORKS, "after_reuse.dot");
        let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] else {
            panic!("A should be registered");
        };
        assert_eq!(reg_a.client_count(), 2, "A should have 2 proxy clients");
        // Both clients share the same net_id
        let net_ids: HashSet<u32> = reg_a
            .replicas()
            .iter()
            .flat_map(|r| r.clients().values().map(|ci| ci.net_id()))
            .collect();
        assert_eq!(
            net_ids.len(),
            1,
            "both clients should share the same net_id"
        );

        let ServiceInfo::Registered(reg_b) = &stack_view(&guard)["B"] else {
            panic!("B should be registered");
        };
        assert_eq!(
            reg_b.client_count(),
            1,
            "B should still have 1 dep client entry"
        );
    }

    // 3. First client times out — network should stay up
    //    C1 created at t=0, C2 at t≈0.5s. Sleep 600ms more → t≈1.1s.
    //    C1 aged 1.1s (> 1s timeout) → expired. C2 aged 0.6s → alive.
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;

    {
        let mut guard = server.services().write().await;
        apply_timeouts(stack_view_mut(&mut guard), server.orchestrator()).await;
        assert_graphviz(&guard, MAX_NETWORKS, "after_first_timeout.dot");

        let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] else {
            panic!("A should be registered");
        };
        assert_eq!(
            reg_a.client_count(),
            1,
            "A should have 1 proxy client after first timeout"
        );
        let ServiceInfo::Registered(reg_b) = &stack_view(&guard)["B"] else {
            panic!("B should be registered");
        };
        assert_eq!(
            reg_b.client_count(),
            1,
            "B dep edge should survive (active_chains > 0)"
        );
    }
    // Net IDs still in use (shared network not torn down)
    assert_net_ids_in_use(&server, 2).await;

    // 4. Second client times out — network should be torn down
    //    C2 needs 0.4s more to reach 1s total. Sleep 500ms for margin.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    {
        let mut guard = server.services().write().await;
        apply_timeouts(stack_view_mut(&mut guard), server.orchestrator()).await;
        assert_graphviz(&guard, MAX_NETWORKS, "after_second_timeout.dot");

        let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] else {
            panic!("A should be registered");
        };
        assert!(
            !reg_a.has_clients(),
            "A should have no clients after both timeouts"
        );
        let ServiceInfo::Registered(reg_b) = &stack_view(&guard)["B"] else {
            panic!("B should be registered");
        };
        assert!(
            !reg_b.has_clients(),
            "B should have no clients after both timeouts"
        );
    }
    // All networks torn down
    assert_net_ids_in_use(&server, 0).await;
}

/// Proxy disconnect tears down both clients sharing a net_id at once.
/// The shared network should be torn down exactly once (dedup).
#[tokio::test]
async fn max_networks_proxy_disconnect() {
    let services = load_fixture(MAX_NETWORKS).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    let ip_map = HashMap::from([("A", ip(1, 1, 1, 1)), ("B", ip(2, 2, 2, 2))]);
    let proxy1 = ip(5, 5, 5, 5);
    register_services(&server, &ip_map, 8080).await;
    server.orchestrator().register_fake_client(proxy1).await;

    setup_proxy_chain(&server, "A", proxy1, "10.0.0.1").await;
    setup_proxy_chain(&server, "A", proxy1, "10.0.0.2").await;
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
        let ServiceInfo::Registered(reg_b) = &stack_view(&guard)["B"] else {
            panic!("B should be registered");
        };
        assert!(
            !reg_b.has_clients(),
            "B should have no clients after proxy disconnect"
        );
    }
    assert_net_ids_in_use(&server, 0).await;
}

/// When max_networks is reached on one proxy, a different proxy with no
/// existing network falls through to new_proxy_chain (soft limit).
#[tokio::test]
async fn max_networks_different_proxy_bypasses() {
    let services = load_fixture(MAX_NETWORKS).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    let ip_map = HashMap::from([("A", ip(1, 1, 1, 1)), ("B", ip(2, 2, 2, 2))]);
    let proxy1 = ip(5, 5, 5, 5);
    let proxy2 = ip(6, 6, 6, 6);
    register_services(&server, &ip_map, 8080).await;
    server.orchestrator().register_fake_client(proxy1).await;
    server.orchestrator().register_fake_client(proxy2).await;

    // First client on proxy1 — creates network
    setup_proxy_chain(&server, "A", proxy1, "10.0.0.1").await;
    assert_net_ids_in_use(&server, 2).await;

    {
        let guard = server.services().read().await;
        assert_graphviz(&guard, MAX_NETWORKS, "after_proxy1_client.dot");
    }

    // proxy2 connects — max_networks=1 is reached, but proxy2 has no existing
    // network to reuse, so it falls through and creates a new one
    setup_proxy_chain(&server, "A", proxy2, "10.0.0.2").await;
    assert_net_ids_in_use(&server, 3).await; // proxy1→A, proxy2→A, A→B (shared dep)

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
// triggers_changed: A entry-point with proxy_dependencies=["B"] and
// triggers=[{5555, ["C"]}]; D entry-point with triggers=[{6666, ["C"]}].
// proxy1→A→B (proxy chain), A→C and D→C (backend chains).
// ===========================================================================

const TRIGGERS_CHANGED: &str = "triggers_changed";

async fn triggers_changed_setup() -> NullnetGrpcImpl {
    let services = load_fixture(TRIGGERS_CHANGED).await;
    let server = NullnetGrpcImpl::new_for_test(services);

    let ip_map = HashMap::from([
        ("A", ip(1, 1, 1, 1)),
        ("B", ip(2, 2, 2, 2)),
        ("C", ip(3, 3, 3, 3)),
        ("D", ip(4, 4, 4, 4)),
    ]);
    let proxy1 = ip(5, 5, 5, 5);
    register_services(&server, &ip_map, 8080).await;
    server.orchestrator().register_fake_client(proxy1).await;

    setup_proxy_chain(&server, "A", proxy1, "10.0.0.1").await;
    trigger_backend_chain(&server, "A", ip(1, 1, 1, 1), 5555).await;
    trigger_backend_chain(&server, "D", ip(4, 4, 4, 4), 6666).await;

    // proxy1→A, A→B, A→C, D→C = 4 IDs
    assert_net_ids_in_use(&server, 4).await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, TRIGGERS_CHANGED, "start.dot");
    drop(guard);

    server
}

/// Drop A's triggers entirely. TriggersChanged{A} tears down A→C.
/// A's proxy chain (proxy1→A, A→B) and D→C survive.
#[tokio::test]
async fn triggers_changed_remove_A_trigger() {
    let server = triggers_changed_setup().await;
    let new_config = load_config(TRIGGERS_CHANGED, "remove_A_trigger.toml").await;

    let mut guard = server.services().write().await;
    apply_config_update(&mut guard, new_config, server.orchestrator()).await;
    assert_graphviz(&guard, TRIGGERS_CHANGED, "after_remove_A_trigger.dot");
    assert!(stack_view(&guard)["A"].triggers().is_empty());
    drop(guard);

    // A→C freed; proxy1→A, A→B, D→C survive = 3 IDs
    assert_net_ids_in_use(&server, 3).await;
}

/// Swap A's chain [C]→[D]. TriggersChanged{A} tears down the existing A→C.
/// New chain isn't auto-rebuilt (no fresh trigger fire). Other chains untouched.
#[tokio::test]
async fn triggers_changed_swap_A_trigger() {
    let server = triggers_changed_setup().await;
    let new_config = load_config(TRIGGERS_CHANGED, "swap_A_trigger.toml").await;

    let mut guard = server.services().write().await;
    apply_config_update(&mut guard, new_config, server.orchestrator()).await;
    assert_graphviz(&guard, TRIGGERS_CHANGED, "after_swap_A_trigger.dot");
    assert_eq!(
        stack_view(&guard)["A"].triggers().get(&5555),
        Some(&vec!["D".to_string()])
    );
    drop(guard);

    assert_net_ids_in_use(&server, 3).await;
}

/// Add a second port to A's triggers. TriggersChanged still fires (map differs)
/// and tears down ALL of A's existing backend chains, even though the addition
/// didn't touch port 5555. The new port=7777 chain is not auto-built.
#[tokio::test]
async fn triggers_changed_add_A_trigger() {
    let server = triggers_changed_setup().await;
    let new_config = load_config(TRIGGERS_CHANGED, "add_A_trigger.toml").await;

    let mut guard = server.services().write().await;
    apply_config_update(&mut guard, new_config, server.orchestrator()).await;
    assert_graphviz(&guard, TRIGGERS_CHANGED, "after_add_A_trigger.dot");
    assert_eq!(stack_view(&guard)["A"].triggers().len(), 2);
    drop(guard);

    assert_net_ids_in_use(&server, 3).await;
}

/// Drop D's trigger. TriggersChanged{D} tears down D→C only.
/// A's chains (proxy + backend) untouched.
#[tokio::test]
async fn triggers_changed_drop_D_trigger() {
    let server = triggers_changed_setup().await;
    let new_config = load_config(TRIGGERS_CHANGED, "drop_D_trigger.toml").await;

    let mut guard = server.services().write().await;
    apply_config_update(&mut guard, new_config, server.orchestrator()).await;
    assert_graphviz(&guard, TRIGGERS_CHANGED, "after_drop_D_trigger.dot");
    assert!(stack_view(&guard)["D"].triggers().is_empty());
    drop(guard);

    assert_net_ids_in_use(&server, 3).await;
}

// ===========================================================================
// backend_reachability_changed: A entry-point with triggers=[{5555, ["C"]}].
// Z is also an entry-point with proxy_dependencies=["A","C"], used to keep A
// and C in the map after A loses its [[services]] entry. Z's chains are not
// activated; it serves only as a config-level reference holder.
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

    trigger_backend_chain(&server, "A", ip(1, 1, 1, 1), 5555).await;

    // A→C = 1 ID
    assert_net_ids_in_use(&server, 1).await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, BACKEND_REACHABILITY_CHANGED, "start.dot");
    drop(guard);

    server
}

/// A loses its [[services]] entry. Detected as TriggersChanged{A} +
/// ReachabilityChanged{A}; both run teardown_all_backend_chains_for(A).
/// A→C is torn down. A stays in the map as an implicit dep (referenced by Z).
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

    assert!(stack_view(&guard).contains_key("A"));
    assert_eq!(stack_view(&guard)["A"].timeout(), None);
    assert!(stack_view(&guard)["A"].triggers().is_empty());
    drop(guard);

    assert_net_ids_in_use(&server, 0).await;
}

// ===========================================================================
// backend_service_unregistered: A entry-point with co-located replicas a1, a2
// at 1.1.1.1, proxy_dependencies=["B"], triggers=[{5555, ["C"]}]. Tests
// selective replica/service removal via apply_services_list while backend
// chains coexist with a proxy chain.
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

    // proxy chain: proxy1 → A → B (lands on a1 — first inserted, least-clients tie)
    setup_proxy_chain(&server, "A", proxy1, "10.0.0.1").await;
    // Backend chains explicit per replica so both a1 and a2 initiate chains
    setup_backend_chain_for_replica(&server, "A", ip(1, 1, 1, 1), Some("a1"), 5555).await;
    setup_backend_chain_for_replica(&server, "A", ip(1, 1, 1, 1), Some("a2"), 5555).await;

    // proxy1→A, A→B, A(a1)→C, A(a2)→C = 4 IDs
    assert_net_ids_in_use(&server, 4).await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, BACKEND_SERVICE_UNREGISTERED, "start.dot");
    drop(guard);

    server
}

/// Re-register host 1.1.1.1 with only a1 (drops a2). ReplicaRemoved{A,1.1.1.1,a2}
/// fires partial teardown: A(a2)→C is torn down via teardown_backend_chain.
/// A(a1)→C and the proxy chain (which landed on a1) survive.
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

    // A(a2)→C freed; proxy1→A, A→B, A(a1)→C survive = 3
    assert_net_ids_in_use(&server, 3).await;
}

/// Re-register host 2.2.2.2 with empty list (drops B). B is the last replica
/// of B → ReplicaRemoved is_last → teardown_invalidated_service. A's proxy
/// chain (proxy1→A, A→B) is torn down via teardown_chain. A's backend chains
/// (A(a1)→C, A(a2)→C) are NOT through B → both survive.
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

    // A's proxy chain torn down — no proxy clients left on A's replicas
    if let ServiceInfo::Registered(reg_a) = &stack_view(&guard)["A"] {
        assert!(!reg_a.has_clients(), "A should have no proxy clients");
    }

    // Backend chains survive: C still has both A(a1) and A(a2) entries
    let ServiceInfo::Registered(reg_c) = &stack_view(&guard)["C"] else {
        panic!("C should be registered");
    };
    assert_eq!(reg_c.client_count(), 2, "both A→C backend chains survive");
    drop(guard);

    // proxy1→A, A→B freed; A(a1)→C, A(a2)→C survive = 2
    assert_net_ids_in_use(&server, 2).await;
}

/// Re-register host 3.3.3.3 with empty list (drops C). C is_last →
/// teardown_invalidated_service cascades to A (which deps_contain C via
/// triggers). A's backend chains AND proxy chain are torn down (the cascade
/// uses ProxyFilter::All on dependents). C ends up Unregistered.
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
        assert!(!reg_a.has_clients(), "A should have no clients");
    }
    drop(guard);

    assert_net_ids_in_use(&server, 0).await;
}

// ===========================================================================
// backend_node_disconnected: same mixed topology as backend_service_unregistered
// (A co-located a1, a2 on 1.1.1.1, B on 2.2.2.2, C on 3.3.3.3, proxy1 on
// 5.5.5.5). Tests handle_node_disconnect's interaction with backend chains.
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

    setup_proxy_chain(&server, "A", proxy1, "10.0.0.1").await;
    setup_backend_chain_for_replica(&server, "A", ip(1, 1, 1, 1), Some("a1"), 5555).await;
    setup_backend_chain_for_replica(&server, "A", ip(1, 1, 1, 1), Some("a2"), 5555).await;

    assert_net_ids_in_use(&server, 4).await;

    let guard = server.services().read().await;
    assert_graphviz(&guard, BACKEND_NODE_DISCONNECTED, "start.dot");
    drop(guard);

    server
}

/// Disconnect 1.1.1.1 (initiator host). Both a1 and a2 vanish at once →
/// ReplicasRemoved{A,1.1.1.1} with is_last=true → teardown_invalidated_service.
/// All chains torn down; A becomes Unregistered.
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

/// Disconnect 3.3.3.3 (backend dep host). C is_last → teardown_invalidated_service
/// cascades to A. Backend chains AND proxy chain torn down (matches the
/// service_unregistered_drop_C semantics).
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
        assert!(!reg_a.has_clients());
    }
    drop(guard);

    assert_net_ids_in_use(&server, 0).await;
}

/// Disconnect proxy host 5.5.5.5. Only ProxyDisconnected fires (no replicas
/// at that IP). proxy1→A and A→B torn down via the proxy filter; backend
/// chains A(a1)→C and A(a2)→C are untouched.
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
        "both backend chains should survive proxy disconnect"
    );
    drop(guard);

    // proxy1→A, A→B freed; A(a1)→C, A(a2)→C survive = 2
    assert_net_ids_in_use(&server, 2).await;
}

// ===========================================================================
// backend_multi_replica: A, D, E are entry-points each backend-triggering to B.
// B has 3 replicas: b1, b2 on 2.2.2.2 (Docker Swarm), standalone on 4.4.4.4.
// Least-clients spreads chains: A→B(b1), D→B(4.4.4.4), E→B(b2). Tests the
// `only_through` filter in teardown_partial_replicas — partial removal of B's
// replicas tears down only chains routing through them.
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

    // Fire backend chains. Least-clients spreads: A→b1, D→4.4.4.4, E→b2.
    trigger_backend_chain(&server, "A", ip(1, 1, 1, 1), 5555).await;
    trigger_backend_chain(&server, "D", ip(5, 5, 5, 5), 6666).await;
    trigger_backend_chain(&server, "E", ip(6, 6, 6, 6), 7777).await;

    // 3 backend chains = 3 NET IDs
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
            "each B replica should host exactly 1 backend chain"
        );
    }
    drop(guard);

    server
}

/// Disconnect 4.4.4.4 (B's standalone replica). ReplicasRemoved{B,4.4.4.4}
/// partial → teardown_partial_replicas finds D-keyed client on B's 4.4.4.4
/// replica and tears down D's chain through B (only_through=Some("B")).
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
/// each upstream's chain through B is torn down via `only_through`.
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
