use crate::events::Event as ServerEvent;
use crate::orchestrator::Orchestrator;
use crate::services::changes::{apply_changes, detect_config_changes};
use crate::services::service_info::ServiceInfo;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use nullnet_grpc_lib::nullnet_grpc::ServiceProtocol;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use serde::Deserialize;
use std::collections::HashMap;
use std::ops::Sub;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc as tokio_mpsc;
use tokio::sync::{Notify, RwLock};
use tokio::time::Instant;

const SERVICES_DIR: &str = "./services";

/// Top-level state: stack name → per-stack service map.
pub(crate) type StackMap = HashMap<String, HashMap<String, ServiceInfo>>;

#[derive(Deserialize)]
pub(crate) struct ServicesToml {
    services: Vec<ServiceToml>,
}

impl ServicesToml {
    pub(crate) async fn load() -> Result<StackMap, Error> {
        Self::load_from_dir(SERVICES_DIR).await
    }

    /// Load every `*.toml` file under `dir`; the file stem is the stack name.
    pub(crate) async fn load_from_dir(dir: &str) -> Result<StackMap, Error> {
        let mut entries = tokio::fs::read_dir(dir).await.handle_err(location!())?;
        let mut stacks: StackMap = HashMap::new();
        while let Some(entry) = entries.next_entry().await.handle_err(location!())? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                continue;
            }
            let Some(stack) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            let services = parse_file(&path).await?;
            println!("Loaded stack '{stack}': {services:?}");
            stacks.insert(stack, services);
        }
        Ok(stacks)
    }

    /// Load a single file as one stack's service map. Used by tests.
    #[cfg(test)]
    pub(crate) async fn load_file(path: &str) -> Result<HashMap<String, ServiceInfo>, Error> {
        parse_file(Path::new(path)).await
    }

    /// Load every stack and fail loudly if any `(protocol, listen_port)` pair
    /// is claimed by more than one service — ports are global on the proxy,
    /// unlike service names which only need to be unique within a stack.
    pub(crate) async fn load_validated() -> Result<StackMap, Error> {
        let stacks = Self::load().await?;
        let conflicts = detect_port_conflicts(&stacks);
        if let Some(first) = conflicts.first() {
            return Err(format!(
                "port mapping conflict: {} other conflict(s); stack '{}' service '{}' and stack '{}' \
                 service '{}' both claim {:?}/{}",
                conflicts.len() - 1,
                first.stack_a,
                first.service_a,
                first.stack_b,
                first.service_b,
                first.protocol,
                first.listen_port
            ))
            .handle_err(location!());
        }
        Ok(stacks)
    }

    /// `config_changed` and `port_mappings_changed` are separate `Notify`s —
    /// each has exactly one consumer (`check_timeouts` and the port-mapping
    /// refresh task, respectively). `Notify::notify_one` wakes at most one
    /// waiter, so sharing a single `Notify` across two consumers would have
    /// them race for each wake-up instead of both reliably observing it.
    pub(crate) async fn watch(
        services: &Arc<RwLock<StackMap>>,
        orchestrator: Orchestrator,
        config_changed: Arc<Notify>,
        port_mappings_changed: Arc<Notify>,
    ) -> Result<(), Error> {
        let services_directory = PathBuf::from(SERVICES_DIR);

        let (tx, mut rx) = tokio_mpsc::unbounded_channel();
        let mut watcher = RecommendedWatcher::new(
            move |event| {
                let _ = tx.send(event);
            },
            Config::default(),
        )
        .handle_err(location!())?;
        watcher
            .watch(&services_directory, RecursiveMode::Recursive)
            .handle_err(location!())?;

        let mut last_update_time = Instant::now().sub(Duration::from_mins(1));

        loop {
            let event = rx.recv().await;
            if event.is_none() {
                println!("File watcher channel closed, stopping watch");
                break;
            }
            if let Some(Ok(Event { kind, .. })) = event
                && matches!(
                    kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                )
            {
                // debounce duplicated events
                if last_update_time.elapsed().as_millis() > 100 {
                    // ensure file changes are propagated
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    match ServicesToml::load().await {
                        Ok(loaded_services) => {
                            let conflicts = detect_port_conflicts(&loaded_services);
                            if conflicts.is_empty() {
                                let services_mut = &mut *services.write().await;
                                apply_config_update(services_mut, loaded_services, &orchestrator)
                                    .await;
                                config_changed.notify_one();
                                port_mappings_changed.notify_one();
                            } else {
                                eprintln!(
                                    "Rejecting services.toml reload: {} port mapping conflict(s)",
                                    conflicts.len()
                                );
                                for c in conflicts {
                                    orchestrator
                                        .events
                                        .emit(ServerEvent::port_mapping_conflict(
                                            c.stack_a,
                                            c.service_a,
                                            c.stack_b,
                                            c.service_b,
                                            format!("{:?}", c.protocol),
                                            c.listen_port,
                                        ))
                                        .await;
                                }
                            }
                        }
                        Err(e) => eprintln!("Failed to reload services.toml: {e:?}"),
                    }
                    last_update_time = Instant::now();
                }
            }
        }

        Ok(())
    }

    pub(crate) fn services_map(self) -> Result<HashMap<String, ServiceInfo>, Error> {
        // Every name referenced in a proxy branch or trigger chain is registered
        // as a discoverable, non-entry-point placeholder (no deps, no timeout) so
        // hosts can register its replicas. The full chain lives on the declaring
        // service — proxy branches are walked from the entry point and trigger
        // chains from the initiator — so deps carry no continuation of their own.
        let mut dep_names: std::collections::HashSet<String> = std::collections::HashSet::new();
        for s in &self.services {
            for dep in s.proxy_dependencies.iter().flatten() {
                dep_names.insert(dep.clone());
            }
            for dep in s.triggers.iter().flat_map(|t| &t.chain) {
                dep_names.insert(dep.clone());
            }
        }

        let mut ret_val: HashMap<String, ServiceInfo> = HashMap::new();
        for name in dep_names {
            ret_val.insert(
                name,
                ServiceInfo::new(
                    Vec::new(),
                    HashMap::new(),
                    None,
                    None,
                    ServiceProtocol::Http,
                    None,
                ),
            );
        }

        // Explicit declarations override any placeholder for the same name,
        // carrying their `timeout` verbatim: `Some` makes the service a
        // proxy-reachable entry point, `None` (omitted) leaves it backend-only.
        // A declared service can thus host triggers without being reachable.
        for s in self.services {
            let protocol = s
                .protocol
                .map_or(ServiceProtocol::Http, ServiceProtocol::from);
            match (protocol, s.listen_port) {
                (ServiceProtocol::Http, Some(_)) => {
                    return Err(format!(
                        "service '{}': 'listen_port' is only valid for protocol \"tcp\"/\"udp\", not \"http\"",
                        s.name
                    ))
                    .handle_err(location!());
                }
                (ServiceProtocol::Tcp | ServiceProtocol::Udp, None) => {
                    return Err(format!(
                        "service '{}': protocol \"tcp\"/\"udp\" requires 'listen_port'",
                        s.name
                    ))
                    .handle_err(location!());
                }
                _ => {}
            }
            let triggers = s.triggers.into_iter().map(|t| (t.port, t.chain)).collect();
            ret_val.insert(
                s.name,
                ServiceInfo::new(
                    s.proxy_dependencies,
                    triggers,
                    s.timeout,
                    s.max_networks,
                    protocol,
                    s.listen_port,
                ),
            );
        }

        Ok(ret_val)
    }
}

/// Two services (possibly in different stacks) that both declared the same
/// `(protocol, listen_port)` pair. Ports are global on the proxy, so unlike
/// service names — which only need to be unique within a stack — these must
/// be unique across every stack.
pub(crate) struct PortConflict {
    pub(crate) stack_a: String,
    pub(crate) service_a: String,
    pub(crate) stack_b: String,
    pub(crate) service_b: String,
    pub(crate) protocol: ServiceProtocol,
    pub(crate) listen_port: u16,
}

/// Scan every stack for `(protocol, listen_port)` pairs claimed by more than
/// one service. `Http` services are excluded — they have no `listen_port`.
pub(crate) fn detect_port_conflicts(stacks: &StackMap) -> Vec<PortConflict> {
    let mut claimed: HashMap<(ServiceProtocol, u16), (String, String)> = HashMap::new();
    let mut conflicts = Vec::new();
    for (stack, services) in stacks {
        for (name, info) in services {
            let Some(listen_port) = info.listen_port() else {
                continue;
            };
            let key = (info.protocol(), listen_port);
            match claimed.get(&key) {
                Some((other_stack, other_service)) => conflicts.push(PortConflict {
                    stack_a: other_stack.clone(),
                    service_a: other_service.clone(),
                    stack_b: stack.clone(),
                    service_b: name.clone(),
                    protocol: key.0,
                    listen_port,
                }),
                None => {
                    claimed.insert(key, (stack.clone(), name.clone()));
                }
            }
        }
    }
    conflicts
}

async fn parse_file(path: &Path) -> Result<HashMap<String, ServiceInfo>, Error> {
    let str_repr = tokio::fs::read_to_string(path)
        .await
        .handle_err(location!())?;
    let parsed: ServicesToml = toml::from_str(&str_repr).handle_err(location!())?;
    parsed.services_map()
}

pub(crate) async fn apply_config_update(
    services: &mut StackMap,
    loaded_services: StackMap,
    orchestrator: &Orchestrator,
) {
    // Stacks that disappeared from config: tear down everything in them.
    let removed_stacks: Vec<String> = services
        .keys()
        .filter(|s| !loaded_services.contains_key(*s))
        .cloned()
        .collect();
    for stack in removed_stacks {
        orchestrator
            .events
            .emit(ServerEvent::config_stack_removed(stack.clone()))
            .await;
        if let Some(stack_map) = services.get_mut(&stack) {
            // Mark every service as removed so apply_changes tears down all
            // chains and replicas before we drop the stack entirely.
            let names: Vec<String> = stack_map.keys().cloned().collect();
            let changes = names
                .into_iter()
                .map(|name| crate::services::changes::ServiceChange::Removed { name })
                .collect();
            apply_changes(changes, stack_map, None, orchestrator, &stack).await;
        }
        services.remove(&stack);
    }

    // Stacks that exist in both: per-service diff. Then stacks new in config:
    // insert as empty maps and let the merge tail in apply_changes populate.
    for (stack, loaded_stack) in loaded_services {
        let stack_map = services.entry(stack.clone()).or_default();
        let changes = detect_config_changes(stack_map, &loaded_stack);
        apply_changes(
            changes,
            stack_map,
            Some(&loaded_stack),
            orchestrator,
            &stack,
        )
        .await;
        orchestrator
            .events
            .emit(ServerEvent::config_reloaded(stack))
            .await;
    }
}

#[derive(Deserialize)]
struct ServiceToml {
    name: String,
    /// Proxy-reachability for this service. Present → reachable entry point
    /// with this per-client timeout in seconds (0 disables the timeout).
    /// Omitted → not proxy-reachable (backend-only); the service is still
    /// declarable to host triggers or backend deps.
    timeout: Option<u64>,
    /// Independent dep chains walked on proxy-triggered setup. Each inner array
    /// is one linear branch; all branches are brought up in parallel.
    #[serde(default)]
    proxy_dependencies: Vec<Vec<String>>,
    /// Backend-triggered chains: each entry pairs a port observed by the
    /// service host with the linear chain to bring up. One chain per port.
    #[serde(default)]
    triggers: Vec<TriggerToml>,
    /// Maximum number of networks that can be created for this service.
    /// Applies to proxy chains only (backend chains are unbounded).
    /// When the limit is reached, new proxy clients reuse an existing network
    /// on the same proxy node instead of creating a new one.
    max_networks: Option<u32>,
    /// Protocol this service is reached over via the proxy. Omitted defaults
    /// to `http` (Host-header routing on the shared 80/443 listeners).
    /// `tcp`/`udp` each require `listen_port` — the proxy binds that port
    /// directly and forwards raw traffic to this service.
    protocol: Option<ProtocolToml>,
    /// External port the proxy listens on for this service. Required (and
    /// only meaningful) when `protocol` is `tcp` or `udp`.
    listen_port: Option<u16>,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ProtocolToml {
    Http,
    Tcp,
    Udp,
}

impl From<ProtocolToml> for ServiceProtocol {
    fn from(value: ProtocolToml) -> Self {
        match value {
            ProtocolToml::Http => ServiceProtocol::Http,
            ProtocolToml::Tcp => ServiceProtocol::Tcp,
            ProtocolToml::Udp => ServiceProtocol::Udp,
        }
    }
}

#[derive(Deserialize)]
struct TriggerToml {
    port: u16,
    #[serde(default)]
    chain: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_and_implicit_services() {
        let toml_str = r#"
[[services]]
name = "color.com"
timeout = 0
proxy_dependencies = [["pre.fs.color.com", "fs.color.com"]]

[[services.triggers]]
port = 5555
chain = ["ts.color.com", "deeper.dep"]

[[services]]
name = "fs.color.com"
timeout = 30
"#;
        let parsed: ServicesToml = toml::from_str(toml_str).unwrap();
        let map = parsed.services_map().unwrap();

        // explicit entry points keep their configured timeout
        assert_eq!(map["color.com"].timeout(), Some(0));
        assert_eq!(map["fs.color.com"].timeout(), Some(30));

        // every name referenced in a proxy_dependencies list or trigger chain
        // is implicitly added with timeout=None (registrable as a dep, not an
        // entry point), regardless of its position in the chain
        assert_eq!(map["pre.fs.color.com"].timeout(), None);
        assert_eq!(map["ts.color.com"].timeout(), None);
        assert_eq!(map["deeper.dep"].timeout(), None);
    }

    #[test]
    fn declared_service_without_timeout_is_unreachable_but_keeps_triggers() {
        // A declared service may omit `timeout` to stay off the proxy while
        // still hosting backend trigger chains.
        let toml_str = r#"
[[services]]
name = "backend.only"
proxy_dependencies = [["dep.a"]]

[[services.triggers]]
port = 5555
chain = ["dep.b"]
"#;
        let parsed: ServicesToml = toml::from_str(toml_str).unwrap();
        let map = parsed.services_map().unwrap();

        // Not proxy-reachable...
        assert_eq!(map["backend.only"].timeout(), None);
        // ...yet it carries its triggers and proxy deps verbatim.
        assert_eq!(map["backend.only"].triggers()[&5555], vec!["dep.b"]);
        assert_eq!(map["backend.only"].proxy_deps(), vec![vec!["dep.a"]]);
    }

    #[test]
    fn parses_parallel_proxy_branches() {
        let toml_str = r#"
[[services]]
name = "app"
timeout = 0
proxy_dependencies = [["a", "b"], ["c", "d"]]
"#;
        let parsed: ServicesToml = toml::from_str(toml_str).unwrap();
        let map = parsed.services_map().unwrap();

        // The entry point keeps both branches verbatim.
        assert_eq!(
            map["app"].proxy_deps(),
            vec![
                vec!["a".to_string(), "b".to_string()],
                vec!["c".to_string(), "d".to_string()],
            ]
        );
        // Every dep across both branches is registered.
        for name in ["a", "b", "c", "d"] {
            assert!(map.contains_key(name), "{name} not registered");
            assert_eq!(map[name].timeout(), None);
        }
        // Deps are bare placeholders — the full chain lives on the entry point,
        // so no intermediate dep carries a continuation of its own.
        for name in ["a", "b", "c", "d"] {
            assert!(
                map[name].proxy_deps().is_empty(),
                "{name} should carry no deps"
            );
        }
    }

    #[tokio::test]
    async fn loads_each_toml_as_its_own_stack() {
        let dir = std::env::temp_dir().join(format!("nullnet-stack-test-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let stack_a = r#"
[[services]]
name = "a.svc"
timeout = 0
proxy_dependencies = [["a.dep"]]
"#;
        let stack_b = r#"
[[services]]
name = "b.svc"
timeout = 30
"#;
        tokio::fs::write(dir.join("alpha.toml"), stack_a)
            .await
            .unwrap();
        tokio::fs::write(dir.join("bravo.toml"), stack_b)
            .await
            .unwrap();
        // unrelated file is ignored
        tokio::fs::write(dir.join("notes.txt"), "ignored")
            .await
            .unwrap();

        let stacks = ServicesToml::load_from_dir(dir.to_str().unwrap())
            .await
            .expect("load");

        assert_eq!(stacks.len(), 2);
        assert!(stacks["alpha"].contains_key("a.svc"));
        assert!(stacks["alpha"].contains_key("a.dep"));
        assert!(stacks["bravo"].contains_key("b.svc"));
        // stacks are isolated: alpha's deps aren't bleeding into bravo
        assert!(!stacks["bravo"].contains_key("a.dep"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[test]
    fn tcp_udp_services_parse_with_listen_port() {
        let toml_str = r#"
[[services]]
name = "redis.internal"
timeout = 0
protocol = "tcp"
listen_port = 6379

[[services]]
name = "dns.internal"
timeout = 0
protocol = "udp"
listen_port = 53
"#;
        let parsed: ServicesToml = toml::from_str(toml_str).unwrap();
        let map = parsed.services_map().unwrap();

        assert_eq!(map["redis.internal"].protocol(), ServiceProtocol::Tcp);
        assert_eq!(map["redis.internal"].listen_port(), Some(6379));
        assert_eq!(map["dns.internal"].protocol(), ServiceProtocol::Udp);
        assert_eq!(map["dns.internal"].listen_port(), Some(53));
    }

    #[test]
    fn http_service_defaults_with_no_listen_port() {
        let toml_str = r#"
[[services]]
name = "color.com"
timeout = 0
"#;
        let parsed: ServicesToml = toml::from_str(toml_str).unwrap();
        let map = parsed.services_map().unwrap();

        assert_eq!(map["color.com"].protocol(), ServiceProtocol::Http);
        assert_eq!(map["color.com"].listen_port(), None);
    }

    #[test]
    fn tcp_protocol_without_listen_port_is_rejected() {
        let toml_str = r#"
[[services]]
name = "redis.internal"
timeout = 0
protocol = "tcp"
"#;
        let parsed: ServicesToml = toml::from_str(toml_str).unwrap();
        assert!(parsed.services_map().is_err());
    }

    #[test]
    fn http_protocol_with_listen_port_is_rejected() {
        let toml_str = r#"
[[services]]
name = "color.com"
timeout = 0
listen_port = 6379
"#;
        let parsed: ServicesToml = toml::from_str(toml_str).unwrap();
        assert!(parsed.services_map().is_err());
    }

    #[test]
    fn detect_port_conflicts_flags_cross_stack_collision() {
        let mut alpha = HashMap::new();
        alpha.insert(
            "redis.a".to_string(),
            ServiceInfo::new(
                vec![],
                HashMap::new(),
                Some(0),
                None,
                ServiceProtocol::Tcp,
                Some(6379),
            ),
        );
        let mut bravo = HashMap::new();
        bravo.insert(
            "redis.b".to_string(),
            ServiceInfo::new(
                vec![],
                HashMap::new(),
                Some(0),
                None,
                ServiceProtocol::Tcp,
                Some(6379),
            ),
        );
        let stacks: StackMap =
            HashMap::from([("alpha".to_string(), alpha), ("bravo".to_string(), bravo)]);

        let conflicts = detect_port_conflicts(&stacks);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].listen_port, 6379);
        assert_eq!(conflicts[0].protocol, ServiceProtocol::Tcp);
    }

    #[test]
    fn detect_port_conflicts_allows_same_port_different_protocol() {
        let mut alpha = HashMap::new();
        alpha.insert(
            "dns.tcp".to_string(),
            ServiceInfo::new(
                vec![],
                HashMap::new(),
                Some(0),
                None,
                ServiceProtocol::Tcp,
                Some(53),
            ),
        );
        alpha.insert(
            "dns.udp".to_string(),
            ServiceInfo::new(
                vec![],
                HashMap::new(),
                Some(0),
                None,
                ServiceProtocol::Udp,
                Some(53),
            ),
        );
        let stacks: StackMap = HashMap::from([("alpha".to_string(), alpha)]);

        assert!(detect_port_conflicts(&stacks).is_empty());
    }
}
