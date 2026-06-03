use crate::env::TIMEOUT;
use crate::events::Event as ServerEvent;
use crate::orchestrator::Orchestrator;
use crate::services::changes::{apply_changes, detect_config_changes};
use crate::services::service_info::ServiceInfo;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
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

    pub(crate) async fn watch(
        services: &Arc<RwLock<StackMap>>,
        orchestrator: Orchestrator,
        config_changed: Arc<Notify>,
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
                    if let Ok(loaded_services) = ServicesToml::load().await {
                        let services_mut = &mut *services.write().await;
                        apply_config_update(services_mut, loaded_services, &orchestrator).await;
                        config_changed.notify_one();
                    }
                    last_update_time = Instant::now();
                }
            }
        }

        Ok(())
    }

    pub(crate) fn services_map(self) -> HashMap<String, ServiceInfo> {
        // Proxy: last-write-wins per dep (a name referenced from multiple
        // branches/services has its tail overwritten by the last processor).
        // Each intermediate dep is registered as a single-branch service whose
        // chain is the remainder of its own branch, so the walkers can
        // reconstruct deep edges node-by-node.
        let mut proxy_accum: HashMap<String, Vec<Vec<String>>> = HashMap::new();
        // Trigger-chain deps: discoverable as (non-entry-point) services so
        // hosts can register replicas of them.
        let mut trigger_dep_names: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for s in &self.services {
            for branch in &s.proxy_dependencies {
                for d in branch {
                    let tail = tail_after(branch, d);
                    // A leaf (empty tail) registers as a service with no deps;
                    // otherwise its remainder is stored as a single branch.
                    let stored = if tail.is_empty() {
                        Vec::new()
                    } else {
                        vec![tail]
                    };
                    proxy_accum.insert(d.clone(), stored);
                }
            }
            for t in &s.triggers {
                for dep in &t.chain {
                    trigger_dep_names.insert(dep.clone());
                }
            }
        }

        let mut ret_val: HashMap<String, ServiceInfo> = HashMap::new();
        for (name, proxy) in proxy_accum {
            ret_val.insert(name, ServiceInfo::new(proxy, HashMap::new(), None, None));
        }
        for name in trigger_dep_names {
            ret_val
                .entry(name)
                .or_insert_with(|| ServiceInfo::new(Vec::new(), HashMap::new(), None, None));
        }

        // Explicit declarations override any implicit entries for the same
        // name and are treated as entry points (`Some(timeout)`). To register a
        // service as a backend dep without making it an entry point, do not
        // declare it explicitly — listing it in a `triggers.chain` is enough.
        for s in self.services {
            let triggers = s.triggers.into_iter().map(|t| (t.port, t.chain)).collect();
            ret_val.insert(
                s.name,
                ServiceInfo::new(
                    s.proxy_dependencies,
                    triggers,
                    Some(s.timeout.unwrap_or(*TIMEOUT)),
                    s.max_networks,
                ),
            );
        }

        ret_val
    }
}

async fn parse_file(path: &Path) -> Result<HashMap<String, ServiceInfo>, Error> {
    let str_repr = tokio::fs::read_to_string(path)
        .await
        .handle_err(location!())?;
    let parsed: ServicesToml = toml::from_str(&str_repr).handle_err(location!())?;
    Ok(parsed.services_map())
}

/// Return the elements of `slice` that come after the first occurrence of `elem`.
fn tail_after(slice: &[String], elem: &str) -> Vec<String> {
    slice
        .iter()
        .position(|d| d == elem)
        .map(|i| slice[i + 1..].to_vec())
        .unwrap_or_default()
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
    /// Per-service entry timeout in seconds for proxy clients. If omitted,
    /// defaults to the global `TIMEOUT` env var (or 60s). A value of 0
    /// disables the timeout. Any explicit declaration is treated as an entry
    /// point; backend deps without a proxy-reachable role should be left out
    /// of explicit declarations and picked up implicitly via trigger chains.
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
        let map = parsed.services_map();

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
    fn parses_parallel_proxy_branches() {
        let toml_str = r#"
[[services]]
name = "app"
timeout = 0
proxy_dependencies = [["a", "b"], ["c", "d"]]
"#;
        let parsed: ServicesToml = toml::from_str(toml_str).unwrap();
        let map = parsed.services_map();

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
        // Each intermediate dep stores the remainder of its own branch as a
        // single branch; leaves store nothing.
        assert_eq!(map["a"].proxy_deps(), vec![vec!["b".to_string()]]);
        assert_eq!(map["c"].proxy_deps(), vec![vec!["d".to_string()]]);
        assert!(map["b"].proxy_deps().is_empty());
        assert!(map["d"].proxy_deps().is_empty());
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
}
