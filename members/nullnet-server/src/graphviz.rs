use crate::env::NET_TYPE;
use crate::services::clients::ClientInfo;
use crate::services::input::StackMap;
use crate::services::service_info::ServiceInfo;
use nullnet_liberror::{ErrorHandler, Location, location};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Set of `(service_name, replica_ip, docker_container)` triples that are
/// acting as a client in at least one dependency edge. Used to mark replicas
/// as "active" even when their own client map is empty.
fn initiators(
    services: &HashMap<String, ServiceInfo>,
) -> HashSet<(String, IpAddr, Option<String>)> {
    services
        .values()
        .filter_map(|info| {
            if let ServiceInfo::Registered(reg) = info {
                Some(reg.replicas().iter().flat_map(|r| r.clients().keys()))
            } else {
                None
            }
        })
        .flatten()
        .filter_map(|c| {
            c.replica_identity()
                .map(|(ip, docker)| (c.name().to_string(), ip, docker.map(String::from)))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Graphviz dot output
// ---------------------------------------------------------------------------

pub(crate) fn render_graphviz(services: &HashMap<String, ServiceInfo>) -> String {
    let mut entries: Vec<_> = services.iter().collect();
    entries.sort_by_key(|(name, _)| *name);

    let initiators = initiators(services);

    let mut graphviz = String::from(
        "digraph G {\n\
            \tbgcolor=grey10;\n\
            \tnode [color=white, fontcolor=white];\n\
            \tedge [color=white, fontcolor=white, fontsize=9, labelangle=180, labeldistance=0.8];\n\n",
    );
    for (name, info) in entries {
        let style = info.graphviz_style();
        let label = info.graphviz_label(name, &initiators);
        let _ =
            writeln!(graphviz, "\t\"{name}\" [label=<{label}>] {style};").handle_err(location!());
        if let ServiceInfo::Registered(registered) = info {
            let mut edges: Vec<_> = registered
                .replicas()
                .iter()
                .flat_map(|replica| replica.clients().iter())
                .collect();
            edges.sort_by_key(|(c, _)| c.display_name());
            for (c, ci) in edges {
                let c_name = c.display_name();
                let edge_label = ci.graphviz_edge_label(false);
                let _ = writeln!(graphviz, "\t\"{c_name}\" -> \"{name}\" {edge_label};")
                    .handle_err(location!());
            }
        }
        graphviz.push('\n');
    }
    graphviz = graphviz.trim().to_string();
    graphviz.push_str("\n}\n");
    graphviz
}

const GRAPHS_DIR: &str = "./graphs";

pub(crate) async fn generate_graphviz(services: Arc<RwLock<StackMap>>) {
    let mut previously_written: HashSet<PathBuf> = HashSet::new();
    loop {
        let _ = tokio::fs::create_dir_all(GRAPHS_DIR)
            .await
            .handle_err(location!());
        let snapshot = services.read().await.clone();
        let mut current: HashSet<PathBuf> = HashSet::new();
        for (stack, stack_map) in &snapshot {
            let graphviz = render_graphviz(stack_map);
            let path = Path::new(GRAPHS_DIR).join(format!("{stack}.dot"));
            let _ = tokio::fs::write(&path, graphviz)
                .await
                .handle_err(location!());
            current.insert(path);
        }
        // Remove .dot files written for stacks that no longer exist.
        for stale in previously_written.difference(&current) {
            let _ = tokio::fs::remove_file(stale).await;
        }
        previously_written = current;

        println!("Regenerated graphviz for {} stack(s)", snapshot.len());

        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

impl ServiceInfo {
    fn graphviz_label(
        &self,
        name: &str,
        initiators: &HashSet<(String, IpAddr, Option<String>)>,
    ) -> String {
        if let ServiceInfo::Registered(reg) = self {
            let total = reg.replicas().len();
            let active = reg
                .replicas()
                .iter()
                .filter(|r| {
                    !r.clients().is_empty()
                        || initiators.contains(&(
                            name.to_string(),
                            r.ip(),
                            r.docker_container().map(String::from),
                        ))
                })
                .count();
            let paused = reg.replicas().iter().filter(|r| r.suspended()).count();
            // HTML-like label: counts on their own line at edge-label size (9) so
            // the node stays narrow and the name keeps the default size.
            return format!(
                "{name}<BR/><FONT POINT-SIZE=\"9\">{active} active / {paused} paused / {total} total</FONT>"
            );
        }
        name.to_string()
    }

    fn graphviz_style(&self) -> &'static str {
        let is_entry_point = self.timeout().is_some();
        match self {
            ServiceInfo::Unregistered(_) if is_entry_point => "[style=solid, color=red]",
            ServiceInfo::Unregistered(_) => "[style=dashed, color=red]",
            ServiceInfo::Registered(_) if is_entry_point => "[style=solid, color=green]",
            ServiceInfo::Registered(_) => "[style=dashed, color=green]",
        }
    }
}

impl ClientInfo {
    fn graphviz_edge_label(&self, show_ends: bool) -> String {
        let client_br = self.client_net();
        let server_br = self.server_net();
        let net_id = self.net_id();
        let time_ms = self.time_ms();
        let net_type_str = NET_TYPE.as_str_name();
        if show_ends {
            format!(
                "[label=\"{net_type_str} {net_id} [{time_ms}ms]\", taillabel=\"{client_br}\", headlabel=\"{server_br}\"]"
            )
        } else {
            format!("[label=\"{net_type_str} {net_id} [{time_ms}ms]\"]")
        }
    }
}

// ---------------------------------------------------------------------------
// JSON graph output
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(crate) struct GraphJson {
    nodes: Vec<GraphNodeJson>,
    edges: Vec<GraphEdgeJson>,
}

#[derive(Serialize)]
struct GraphNodeJson {
    id: String,
    registered: bool,
    entry_point: bool,
    replica_count: usize,
    active_replica_count: usize,
    paused_replica_count: usize,
}

#[derive(Serialize)]
struct GraphEdgeJson {
    from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    via_proxy: Option<String>,
    to: String,
    net_id: u32,
    setup_ms: u128,
}

pub(crate) fn render_graph_json(services: &HashMap<String, ServiceInfo>) -> GraphJson {
    let initiators = initiators(services);

    let mut nodes: Vec<GraphNodeJson> = services
        .iter()
        .map(|(name, info)| {
            let registered = matches!(info, ServiceInfo::Registered(_));
            let entry_point = info.timeout().is_some();
            let (replica_count, active_replica_count, paused_replica_count) =
                if let ServiceInfo::Registered(reg) = info {
                    let total = reg.replicas().len();
                    let active = reg
                        .replicas()
                        .iter()
                        .filter(|r| {
                            !r.clients().is_empty()
                                || initiators.contains(&(
                                    name.clone(),
                                    r.ip(),
                                    r.docker_container().map(String::from),
                                ))
                        })
                        .count();
                    let paused = reg.replicas().iter().filter(|r| r.suspended()).count();
                    (total, active, paused)
                } else {
                    (0, 0, 0)
                };
            GraphNodeJson {
                id: name.clone(),
                registered,
                entry_point,
                replica_count,
                active_replica_count,
                paused_replica_count,
            }
        })
        .collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));

    let mut edges: Vec<GraphEdgeJson> = services
        .iter()
        .filter_map(|(name, info)| {
            if let ServiceInfo::Registered(reg) = info {
                Some((name, reg))
            } else {
                None
            }
        })
        .flat_map(|(name, reg)| {
            reg.replicas().iter().flat_map(move |replica| {
                replica.clients().iter().map(move |(c, ci)| GraphEdgeJson {
                    from: c.name().to_string(),
                    via_proxy: c.is_proxy().map(|ip| ip.to_string()),
                    to: name.clone(),
                    net_id: ci.net_id(),
                    setup_ms: ci.time_ms(),
                })
            })
        })
        .collect();
    edges.sort_by(|a, b| a.from.cmp(&b.from).then(a.to.cmp(&b.to)));

    GraphJson { nodes, edges }
}
