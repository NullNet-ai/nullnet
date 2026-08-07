use crate::events::Event as ServerEvent;
use crate::orchestrator::Orchestrator;
use crate::services::changes::{ServiceChange, apply_changes, detect_config_changes};
use crate::services::clients::Client;
use crate::services::service_info::{CountryPolicy, ServiceInfo};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use nullnet_grpc_lib::nullnet_grpc::ServiceProtocol;
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::Ipv4Addr;
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

/// A service's host-match keys + backend port, used to join client observations
/// to `(stack, service)`. Built from config; an observation may match several.
#[derive(Clone, Debug)]
pub(crate) struct MatchEntry {
    pub(crate) name: String,
    pub(crate) port: u16,
    pub(crate) docker_container: Option<String>,
    pub(crate) process_path: Option<String>,
}

/// Stack name → its services' match entries. Rebuilt on every load/reload.
pub(crate) type MatchIndex = HashMap<String, Vec<MatchEntry>>;

/// A parsed, validated `[[route]]` entry: an HTTP(S) dispatch rule matching
/// `host` + a `path` prefix, to either an existing (in-stack, http-protocol)
/// service or a literal redirect. See docs/http-path-routing-design.md.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RouteEntry {
    pub(crate) host: String,
    pub(crate) path: String,
    pub(crate) target: RouteTarget,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RouteTarget {
    Service(String),
    Redirect { to: String, status: u16 },
}

/// Stack name → its explicit route entries. Rebuilt on every load/reload,
/// alongside `StackMap`/`MatchIndex`. Implicit per-service fallback routes
/// (an http service with no explicit route naming it) are *not* stored here
/// — whether one applies depends on every stack's explicit routes, not just
/// this one's, so they're derived at bundle-build time
/// (`nullnet_grpc_impl::build_http_route_bundle`).
pub(crate) type RouteMap = HashMap<String, Vec<RouteEntry>>;

/// Validate and convert one stack's raw `[[route]]` entries against its
/// `[[services]]` list: exactly one target (`service`/`redirect_to`) per
/// route, a `service` target must name an in-stack, http-protocol service,
/// and `redirect_status` (when present) must be a supported redirect code.
/// `(host, path)` uniqueness — within this stack or across stacks — is a
/// separate check; see `detect_route_conflicts`.
fn build_route_entries(
    services: &[ServiceToml],
    routes: &[RouteToml],
) -> Result<Vec<RouteEntry>, Error> {
    let mut entries = Vec::with_capacity(routes.len());
    for r in routes {
        let target = match (&r.service, &r.redirect_to) {
            (Some(_), Some(_)) => {
                return Err(format!(
                    "route '{} {}': 'service' and 'redirect_to' are mutually exclusive",
                    r.host, r.path
                ))
                .handle_err(location!());
            }
            (None, None) => {
                return Err(format!(
                    "route '{} {}': exactly one of 'service'/'redirect_to' is required",
                    r.host, r.path
                ))
                .handle_err(location!());
            }
            (Some(service_name), None) => {
                let Some(svc) = services.iter().find(|s| &s.name == service_name) else {
                    return Err(format!(
                        "route '{} {}': service '{service_name}' is not declared in this stack",
                        r.host, r.path
                    ))
                    .handle_err(location!());
                };
                let protocol = svc
                    .protocol
                    .map_or(ServiceProtocol::Http, ServiceProtocol::from);
                if protocol != ServiceProtocol::Http {
                    return Err(format!(
                        "route '{} {}': service '{service_name}' is protocol {protocol:?}, \
                         routes can only target http services",
                        r.host, r.path
                    ))
                    .handle_err(location!());
                }
                if svc.timeout.is_none() {
                    return Err(format!(
                        "route '{} {}': service '{service_name}' has no 'timeout', so it isn't a \
                         proxy-reachable entry point",
                        r.host, r.path
                    ))
                    .handle_err(location!());
                }
                RouteTarget::Service(service_name.clone())
            }
            (None, Some(to)) => {
                let status = r.redirect_status.unwrap_or(301);
                if !matches!(status, 301 | 302 | 307 | 308) {
                    return Err(format!(
                        "route '{} {}': redirect_status {status} must be one of 301/302/307/308",
                        r.host, r.path
                    ))
                    .handle_err(location!());
                }
                RouteTarget::Redirect {
                    to: to.clone(),
                    status,
                }
            }
        };
        entries.push(RouteEntry {
            host: r.host.clone(),
            path: r.path.clone(),
            target,
        });
    }
    Ok(entries)
}

/// Two routes (possibly in different stacks, or the same one) that both
/// claim the same `(host, path)` pair — that pair dispatches to exactly one
/// target on the proxy, so it must be globally unique, the same way
/// `(protocol, listen_port)` already is for tcp/udp.
pub(crate) struct RouteConflict {
    pub(crate) stack_a: String,
    pub(crate) stack_b: String,
    pub(crate) host: String,
    pub(crate) path: String,
}

/// Scan every stack for `(host, path)` pairs claimed by more than one route,
/// including two routes within the same stack.
pub(crate) fn detect_route_conflicts(routes: &RouteMap) -> Vec<RouteConflict> {
    let mut claimed: HashMap<(String, String), String> = HashMap::new();
    let mut conflicts = Vec::new();
    for (stack, entries) in routes {
        for entry in entries {
            let key = (entry.host.clone(), entry.path.clone());
            match claimed.get(&key) {
                Some(other_stack) => conflicts.push(RouteConflict {
                    stack_a: other_stack.clone(),
                    stack_b: stack.clone(),
                    host: entry.host.clone(),
                    path: entry.path.clone(),
                }),
                None => {
                    claimed.insert(key, stack.clone());
                }
            }
        }
    }
    conflicts
}

/// Match entries for one file: only services with a match key are hostable, and
/// such a service must declare a `port`.
fn build_match_entries(services: &[ServiceToml]) -> Result<Vec<MatchEntry>, Error> {
    let mut entries = Vec::new();
    for s in services {
        if s.docker_container.is_none() && s.process_path.is_none() {
            continue;
        }
        let Some(port) = s.port else {
            return Err(format!(
                "service '{}': has a host-match key (docker_container/process_path) but no 'port'",
                s.name
            ))
            .handle_err(location!());
        };
        entries.push(MatchEntry {
            name: s.name.clone(),
            port,
            docker_container: s.docker_container.clone(),
            process_path: s.process_path.clone(),
        });
    }
    Ok(entries)
}

#[derive(Deserialize)]
pub(crate) struct ServicesToml {
    // Defaulted so a stack can be routes-only (e.g. a bare redirect, which
    // needs no backend at all) without an empty `services = []` boilerplate.
    #[serde(default)]
    services: Vec<ServiceToml>,
    #[serde(default, rename = "route")]
    routes: Vec<RouteToml>,
}

impl ServicesToml {
    pub(crate) async fn load() -> Result<(StackMap, MatchIndex, RouteMap), Error> {
        Self::load_from_dir(SERVICES_DIR).await
    }

    /// Load every `*.toml` file under `dir`; the file stem is the stack name.
    /// Returns the service map, the parallel host-match index, and the
    /// parallel route map.
    pub(crate) async fn load_from_dir(
        dir: &str,
    ) -> Result<(StackMap, MatchIndex, RouteMap), Error> {
        let mut entries = tokio::fs::read_dir(dir).await.handle_err(location!())?;
        let mut stacks: StackMap = HashMap::new();
        let mut index: MatchIndex = HashMap::new();
        let mut routes: RouteMap = HashMap::new();
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
            let (services, match_entries, route_entries) = parse_file(&path).await?;
            println!("Loaded stack '{stack}': {services:?}");
            stacks.insert(stack.clone(), services);
            index.insert(stack.clone(), match_entries);
            routes.insert(stack, route_entries);
        }
        Ok((stacks, index, routes))
    }

    /// Load a single file as one stack's service map. Used by tests.
    #[cfg(test)]
    pub(crate) async fn load_file(path: &str) -> Result<HashMap<String, ServiceInfo>, Error> {
        Ok(parse_file(Path::new(path)).await?.0)
    }

    /// Load every stack and fail loudly if any `(protocol, listen_port)` pair
    /// or `(host, path)` route pair is claimed by more than one service/route
    /// — both are global on the proxy, unlike service names which only need
    /// to be unique within a stack.
    pub(crate) async fn load_validated() -> Result<(StackMap, MatchIndex, RouteMap), Error> {
        let (mut stacks, mut index, mut routes) = Self::load().await?;
        // Don't brick the control plane on a conflict (e.g. a bad UI edit or
        // hand-edited file left two stacks claiming the same listen_port/route).
        // Drop the offending stacks and start anyway, logging loudly — the
        // reload path is likewise tolerant, and an operator can fix the files
        // without downtime.
        let conflicts = detect_port_conflicts(&stacks);
        let mut bad: HashSet<String> = HashSet::new();
        for c in &conflicts {
            eprintln!(
                "[config] port conflict on startup: {:?}/{} claimed by '{}'/'{}' and '{}'/'{}' — \
                 dropping these stacks until fixed",
                c.protocol, c.listen_port, c.stack_a, c.service_a, c.stack_b, c.service_b
            );
            bad.insert(c.stack_a.clone());
            bad.insert(c.stack_b.clone());
        }
        let route_conflicts = detect_route_conflicts(&routes);
        for c in &route_conflicts {
            eprintln!(
                "[config] route conflict on startup: '{} {}' claimed by both '{}' and '{}' — \
                 dropping these stacks until fixed",
                c.host, c.path, c.stack_a, c.stack_b
            );
            bad.insert(c.stack_a.clone());
            bad.insert(c.stack_b.clone());
        }
        for stack in &bad {
            stacks.remove(stack);
            index.remove(stack);
            routes.remove(stack);
        }
        Ok((stacks, index, routes))
    }

    /// `config_changed`, `port_mappings_changed`, and `http_routes_changed`
    /// are separate `Notify`s — each has exactly one consumer (`check_timeouts`,
    /// the port-mapping refresh task, and the route-table refresh task,
    /// respectively). `Notify::notify_one` wakes at most one waiter, so
    /// sharing a single `Notify` across consumers would have them race for
    /// each wake-up instead of all reliably observing it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn watch(
        services: &Arc<RwLock<StackMap>>,
        match_index: &Arc<RwLock<MatchIndex>>,
        routes: &Arc<RwLock<RouteMap>>,
        orchestrator: Orchestrator,
        config_changed: Arc<Notify>,
        port_mappings_changed: Arc<Notify>,
        http_routes_changed: Arc<Notify>,
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
                        Ok((loaded_services, loaded_index, loaded_routes)) => {
                            let conflicts = detect_port_conflicts(&loaded_services);
                            let route_conflicts = detect_route_conflicts(&loaded_routes);
                            if conflicts.is_empty() && route_conflicts.is_empty() {
                                let services_mut = &mut *services.write().await;
                                apply_config_update(services_mut, loaded_services, &orchestrator)
                                    .await;
                                *match_index.write().await = loaded_index;
                                *routes.write().await = loaded_routes;
                                config_changed.notify_one();
                                port_mappings_changed.notify_one();
                                http_routes_changed.notify_one();
                            } else {
                                eprintln!(
                                    "Rejecting services.toml reload: {} port mapping conflict(s), \
                                     {} route conflict(s)",
                                    conflicts.len(),
                                    route_conflicts.len()
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
                                for c in route_conflicts {
                                    orchestrator
                                        .events
                                        .emit(ServerEvent::route_conflict(
                                            c.stack_a, c.stack_b, c.host, c.path,
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
                    CountryPolicy::None,
                    CountryPolicy::None,
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
            let egress_policy = country_policy(
                s.egress_blocked_countries,
                s.egress_allowed_countries,
                "egress",
                &s.name,
            )?;
            let ingress_policy = country_policy(
                s.ingress_blocked_countries,
                s.ingress_allowed_countries,
                "ingress",
                &s.name,
            )?;
            // Ingress policy is enforced at the proxy, which only routes reachable
            // entry points. On a backend-only service it would be dead config, so
            // reject it rather than silently ignore.
            if ingress_policy != CountryPolicy::None && s.timeout.is_none() {
                return Err(format!(
                    "service '{}': ingress country policy requires a proxy-reachable service (set a 'timeout')",
                    s.name
                ))
                .handle_err(location!());
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
                    egress_policy,
                    ingress_policy,
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

/// Normalize country codes for case-insensitive matching.
fn upper(list: Vec<String>) -> Vec<String> {
    list.into_iter().map(|c| c.to_uppercase()).collect()
}

/// Build a `CountryPolicy` from one direction's blocked/allowed lists. The two
/// lists are mutually exclusive; `dir` ("ingress"/"egress") only shapes the
/// error message so it names the offending fields.
fn country_policy(
    blocked: Option<Vec<String>>,
    allowed: Option<Vec<String>>,
    dir: &str,
    service: &str,
) -> Result<CountryPolicy, Error> {
    match (blocked, allowed) {
        (Some(_), Some(_)) => Err(format!(
            "service '{service}': '{dir}_blocked_countries' and '{dir}_allowed_countries' are mutually exclusive"
        ))
        .handle_err(location!()),
        (Some(list), None) => Ok(CountryPolicy::Blocked(upper(list))),
        (None, Some(list)) => Ok(CountryPolicy::Allowed(upper(list))),
        (None, None) => Ok(CountryPolicy::None),
    }
}

/// One stack file's parsed, validated contents: its service map, host-match
/// index entries, and route entries.
type ParsedStack = (
    HashMap<String, ServiceInfo>,
    Vec<MatchEntry>,
    Vec<RouteEntry>,
);

/// Single source of truth for "is this stack file valid?": TOML syntax, then the
/// host-match/port, protocol/listen_port, route, and country-list rules. Shared
/// by the disk loader ([`parse_file`]) and the UI save handlers
/// ([`validate_stack_toml`]). Cross-stack port/route conflicts are separate —
/// see [`detect_port_conflicts`]/[`detect_route_conflicts`].
fn parse_stack_content(content: &str) -> Result<ParsedStack, Error> {
    let parsed: ServicesToml = toml::from_str(content).handle_err(location!())?;
    let match_entries = build_match_entries(&parsed.services)?;
    let route_entries = build_route_entries(&parsed.services, &parsed.routes)?;
    Ok((parsed.services_map()?, match_entries, route_entries))
}

/// Validate raw TOML the same way the loader does, returning the per-service
/// map and route list on success or a human-readable error for the UI's
/// parse-status indicator.
pub(crate) fn validate_stack_toml(
    content: &str,
) -> Result<(HashMap<String, ServiceInfo>, Vec<RouteEntry>), String> {
    parse_stack_content(content)
        .map(|(services, _match_entries, routes)| (services, routes))
        .map_err(|e| e.to_str().to_string())
}

async fn parse_file(path: &Path) -> Result<ParsedStack, Error> {
    let str_repr = tokio::fs::read_to_string(path)
        .await
        .handle_err(location!())?;
    parse_stack_content(&str_repr)
}

/// All non-default egress policies, keyed by (stack, service) — the comparable
/// footprint used to detect a policy change across a reload.
fn egress_policies(services: &StackMap) -> BTreeMap<(String, String), CountryPolicy> {
    services
        .iter()
        .flat_map(|(stack, map)| {
            map.iter()
                .filter(|(_, si)| *si.egress_policy() != CountryPolicy::None)
                .map(|(name, si)| ((stack.clone(), name.clone()), si.egress_policy().clone()))
        })
        .collect()
}

/// All non-default ingress policies, keyed by (stack, service) — the comparable
/// footprint used to detect an ingress-policy change across a reload.
fn ingress_policies(services: &StackMap) -> BTreeMap<(String, String), CountryPolicy> {
    services
        .iter()
        .flat_map(|(stack, map)| {
            map.iter()
                .filter(|(_, si)| *si.ingress_policy() != CountryPolicy::None)
                .map(|(name, si)| ((stack.clone(), name.clone()), si.ingress_policy().clone()))
        })
        .collect()
}

/// Tear down every active proxy session whose client country the current ingress
/// policy denies — the same `ForceSessionTeardown` the manual teardown button
/// uses, so the nullnet overlay is cleaned up. tcp/udp streams close when their
/// path drops; HTTP/S is already 403'd per-request.
///
/// Only the services in `changed` (whose ingress policy actually differs on this
/// reload) are re-evaluated, so an unrelated policy edit can't disturb others.
/// Country comes from the cached geo (`geo_get`, no network I/O) — this runs
/// under the `services` write lock, so it must not block; ingress IPs are warmed
/// by `check_ingress`, and an unresolved one is treated per policy (allow-list
/// unknown → deny), consistent with the door check that re-evaluates on reconnect.
async fn teardown_ingress_denied_sessions(
    services: &mut StackMap,
    orchestrator: &Orchestrator,
    changed: &HashSet<(String, String)>,
) {
    // Phase 1: collect (stack, service, client) the reloaded policy now denies.
    let mut denied: Vec<(String, String, Client)> = Vec::new();
    for (stack, stack_map) in services.iter() {
        for (svc_name, info) in stack_map {
            if !changed.contains(&(stack.clone(), svc_name.clone())) {
                continue;
            }
            let policy = info.ingress_policy();
            if *policy == CountryPolicy::None {
                continue;
            }
            let ServiceInfo::Registered(reg) = info else {
                continue;
            };
            for (client, _ci, _ip, _docker) in reg.all_clients_owned() {
                if client.is_proxy().is_none() {
                    continue;
                }
                let Ok(ip) = client.name().parse::<Ipv4Addr>() else {
                    continue;
                };
                let country = orchestrator
                    .geo_get(ip)
                    .and_then(|g| g.country_code)
                    .map(|c| c.to_uppercase());
                if !policy.allows(country.as_deref()) {
                    denied.push((stack.clone(), svc_name.clone(), client));
                }
            }
        }
    }
    // Phase 2: tear each down through the shared teardown path.
    for (stack, name, client) in denied {
        if let Some(stack_map) = services.get_mut(&stack) {
            apply_changes(
                vec![ServiceChange::ForceSessionTeardown { name, client }],
                stack_map,
                None,
                orchestrator,
                &stack,
            )
            .await;
        }
    }
}

pub(crate) async fn apply_config_update(
    services: &mut StackMap,
    loaded_services: StackMap,
    orchestrator: &Orchestrator,
) {
    let policies_before = egress_policies(services);
    let policies_after = egress_policies(&loaded_services);
    let ingress_before = ingress_policies(services);
    let ingress_after = ingress_policies(&loaded_services);
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

    // Any egress country-policy difference → tell clients to re-verdict live
    // flows (they flush verdict caches + conntrack; denied flows die).
    if policies_before != policies_after {
        orchestrator.broadcast_egress_policy_changed().await;
    }

    // Services whose ingress policy actually changed (added or altered — a removed
    // policy is now None and denies nothing, so it needs no teardown). Only these
    // are re-evaluated, so editing one service's policy can't disturb others.
    let changed_ingress: HashSet<(String, String)> = ingress_after
        .iter()
        .filter(|(k, v)| ingress_before.get(*k) != Some(*v))
        .map(|(k, _)| k.clone())
        .collect();
    if !changed_ingress.is_empty() {
        teardown_ingress_denied_sessions(services, orchestrator, &changed_ingress).await;
    }
}

#[derive(Deserialize)]
struct ServiceToml {
    name: String,
    /// Host-match key for a Docker service: the Swarm service label
    /// (`com.docker.swarm.service.name`) or, in standalone mode, the container
    /// name. A running container whose label equals this registers a replica.
    docker_container: Option<String>,
    /// Host-match key for a non-Docker service: the listening process's exe path
    /// (`/proc/<pid>/exe`). A listener with this path registers a replica.
    process_path: Option<String>,
    /// Backend port the overlay/proxy connects to on this service's replicas.
    /// Required when any host-match key is set. Distinct from `listen_port`
    /// (the proxy's external tcp/udp front port).
    port: Option<u16>,
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
    /// Country policies (ISO alpha-2 codes). Within each direction the blocked/
    /// allowed lists are mutually exclusive: `*_blocked_countries` denies the
    /// listed countries (unknown → allow); `*_allowed_countries` permits only the
    /// listed ones (unknown → deny). Egress = destination country of the service's
    /// outbound traffic; ingress = source country of proxy clients reaching it.
    egress_blocked_countries: Option<Vec<String>>,
    egress_allowed_countries: Option<Vec<String>>,
    ingress_blocked_countries: Option<Vec<String>>,
    ingress_allowed_countries: Option<Vec<String>>,
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

/// One `[[route]]` entry: an HTTP(S) location-block-style dispatch rule.
/// `service`/`redirect_to` are mutually exclusive — validated (along with
/// `redirect_status`) in [`build_route_entries`], not here, so a raw parse
/// error and a semantic one both surface through the same caller.
#[derive(Deserialize)]
struct RouteToml {
    host: String,
    #[serde(default = "default_route_path")]
    path: String,
    service: Option<String>,
    redirect_to: Option<String>,
    redirect_status: Option<u16>,
}

fn default_route_path() -> String {
    "/".to_string()
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

        let (stacks, _index, _routes) = ServicesToml::load_from_dir(dir.to_str().unwrap())
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
                CountryPolicy::None,
                CountryPolicy::None,
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
                CountryPolicy::None,
                CountryPolicy::None,
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
                CountryPolicy::None,
                CountryPolicy::None,
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
                CountryPolicy::None,
                CountryPolicy::None,
            ),
        );
        let stacks: StackMap = HashMap::from([("alpha".to_string(), alpha)]);

        assert!(detect_port_conflicts(&stacks).is_empty());
    }

    fn routes_of(toml_str: &str) -> Result<Vec<RouteEntry>, String> {
        parse_stack_content(toml_str)
            .map(|(_, _, routes)| routes)
            .map_err(|e| e.to_str().to_string())
    }

    #[test]
    fn route_targeting_a_declared_http_service_parses() {
        let toml_str = r#"
[[services]]
name = "grafana"
timeout = 30

[[route]]
host = "ops.example.com"
path = "/grafana"
service = "grafana"
"#;
        let routes = routes_of(toml_str).unwrap();
        assert_eq!(
            routes,
            vec![RouteEntry {
                host: "ops.example.com".to_string(),
                path: "/grafana".to_string(),
                target: RouteTarget::Service("grafana".to_string()),
            }]
        );
    }

    #[test]
    fn route_path_defaults_to_slash() {
        let toml_str = r#"
[[services]]
name = "grafana"
timeout = 30

[[route]]
host = "ops.example.com"
service = "grafana"
"#;
        let routes = routes_of(toml_str).unwrap();
        assert_eq!(routes[0].path, "/");
    }

    #[test]
    fn redirect_route_defaults_status_to_301() {
        let toml_str = r#"
[[route]]
host = "old.example.com"
redirect_to = "https://new.example.com/"
"#;
        let routes = routes_of(toml_str).unwrap();
        assert_eq!(
            routes,
            vec![RouteEntry {
                host: "old.example.com".to_string(),
                path: "/".to_string(),
                target: RouteTarget::Redirect {
                    to: "https://new.example.com/".to_string(),
                    status: 301,
                },
            }]
        );
    }

    #[test]
    fn route_with_both_service_and_redirect_is_rejected() {
        let toml_str = r#"
[[services]]
name = "grafana"
timeout = 30

[[route]]
host = "ops.example.com"
service = "grafana"
redirect_to = "https://elsewhere.example.com/"
"#;
        assert!(
            routes_of(toml_str)
                .unwrap_err()
                .contains("mutually exclusive")
        );
    }

    #[test]
    fn route_with_neither_service_nor_redirect_is_rejected() {
        let toml_str = r#"
[[route]]
host = "ops.example.com"
"#;
        assert!(
            routes_of(toml_str)
                .unwrap_err()
                .contains("exactly one of 'service'/'redirect_to'")
        );
    }

    #[test]
    fn route_targeting_undeclared_service_is_rejected() {
        let toml_str = r#"
[[route]]
host = "ops.example.com"
service = "grafana"
"#;
        assert!(
            routes_of(toml_str)
                .unwrap_err()
                .contains("not declared in this stack")
        );
    }

    #[test]
    fn route_targeting_tcp_service_is_rejected() {
        let toml_str = r#"
[[services]]
name = "redis.internal"
timeout = 0
protocol = "tcp"
listen_port = 6379

[[route]]
host = "ops.example.com"
service = "redis.internal"
"#;
        assert!(
            routes_of(toml_str)
                .unwrap_err()
                .contains("routes can only target http services")
        );
    }

    #[test]
    fn route_targeting_non_entry_point_service_is_rejected() {
        let toml_str = r#"
[[services]]
name = "backend.only"

[[route]]
host = "ops.example.com"
service = "backend.only"
"#;
        assert!(
            routes_of(toml_str)
                .unwrap_err()
                .contains("isn't a proxy-reachable entry point")
        );
    }

    #[test]
    fn redirect_route_with_unsupported_status_is_rejected() {
        let toml_str = r#"
[[route]]
host = "old.example.com"
redirect_to = "https://new.example.com/"
redirect_status = 200
"#;
        assert!(
            routes_of(toml_str)
                .unwrap_err()
                .contains("must be one of 301/302/307/308")
        );
    }

    #[test]
    fn detect_route_conflicts_flags_cross_stack_collision() {
        let alpha = vec![RouteEntry {
            host: "ops.example.com".to_string(),
            path: "/".to_string(),
            target: RouteTarget::Service("a".to_string()),
        }];
        let bravo = vec![RouteEntry {
            host: "ops.example.com".to_string(),
            path: "/".to_string(),
            target: RouteTarget::Service("b".to_string()),
        }];
        let routes: RouteMap =
            HashMap::from([("alpha".to_string(), alpha), ("bravo".to_string(), bravo)]);

        let conflicts = detect_route_conflicts(&routes);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].host, "ops.example.com");
        assert_eq!(conflicts[0].path, "/");
    }

    #[test]
    fn detect_route_conflicts_allows_same_host_different_paths() {
        let alpha = vec![
            RouteEntry {
                host: "ops.example.com".to_string(),
                path: "/a".to_string(),
                target: RouteTarget::Service("a".to_string()),
            },
            RouteEntry {
                host: "ops.example.com".to_string(),
                path: "/b".to_string(),
                target: RouteTarget::Service("b".to_string()),
            },
        ];
        let routes: RouteMap = HashMap::from([("alpha".to_string(), alpha)]);

        assert!(detect_route_conflicts(&routes).is_empty());
    }
}
