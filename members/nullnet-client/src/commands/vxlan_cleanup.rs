use futures::TryStreamExt;
use rtnetlink::packet_route::link::LinkAttribute;
use rtnetlink::{Handle, LinkUnspec};
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::sync::{OwnedMutexGuard, mpsc, oneshot};

// A single worker owns this temporary group; only retiring links enter it.
const RETIRING_GROUP: u32 = 0x4e40_0000;
const DELETE_BATCH: usize = 128;
static ACTIVE_SETUPS: AtomicUsize = AtomicUsize::new(0);
type Request = (
    u32,
    Arc<OwnedMutexGuard<()>>,
    oneshot::Sender<Result<(), String>>,
    Option<super::endpoint_io::Cleanup>,
);
static CLEANUP: OnceLock<mpsc::Sender<Request>> = OnceLock::new();

pub(super) struct SetupGuard;

pub(super) fn track_setup() -> SetupGuard {
    ACTIVE_SETUPS.fetch_add(1, Ordering::Relaxed);
    SetupGuard
}

impl Drop for SetupGuard {
    fn drop(&mut self) {
        ACTIVE_SETUPS.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(super) async fn remove(
    handle: &Handle,
    group: u32,
    guard: Arc<OwnedMutexGuard<()>>,
) -> Result<(), String> {
    remove_with_cleanup(handle, group, guard, None).await
}

pub(super) async fn remove_with_cleanup(
    handle: &Handle,
    group: u32,
    guard: Arc<OwnedMutexGuard<()>>,
    cleanup: Option<super::endpoint_io::Cleanup>,
) -> Result<(), String> {
    let sender = CLEANUP.get_or_init(|| {
        let (sender, mut receiver) = mpsc::channel::<Request>(1024);
        let handle = handle.clone();
        tokio::spawn(async move {
            let mut batch = Vec::with_capacity(DELETE_BATCH);
            while let Some(first) = receiver.recv().await {
                batch.push(first);
                while batch.len() < DELETE_BATCH {
                    match receiver.try_recv() {
                        Ok(request) => batch.push(request),
                        Err(_) => break,
                    }
                }
                let mut result = Ok(());
                let mut offset = 0;
                while offset < batch.len() {
                    // Keep RTNL deletion batches small while setup is active.
                    let limit = if ACTIVE_SETUPS.load(Ordering::Relaxed) == 0 {
                        DELETE_BATCH
                    } else {
                        16
                    };
                    let end = (offset + limit).min(batch.len());
                    let groups = batch[offset..end]
                        .iter()
                        .map(|(group, _, _, _)| *group)
                        .collect();
                    let names: Option<Vec<_>> = batch[offset..end]
                        .iter()
                        .map(|(_, _, _, cleanup)| cleanup.as_ref().map(|c| c.root_names.clone()))
                        .collect();
                    let names = names.map(|names| names.into_iter().flatten().collect());
                    result = remove_groups(&handle, groups, names).await;
                    if result.is_err() {
                        break;
                    }
                    offset = end;
                    tokio::task::yield_now().await;
                }
                if result.is_ok() {
                    let cleanups: Vec<_> =
                        batch.iter().filter_map(|(_, _, _, c)| c.clone()).collect();
                    if !cleanups.is_empty() {
                        result = super::endpoint_io::cleanup_batch(cleanups)
                            .await
                            .map_err(|e| e.to_string());
                    }
                }
                for (_, _guard, response, _) in batch.drain(..) {
                    let _ = response.send(result.clone());
                }
            }
        });
        sender
    });
    let (response, received) = oneshot::channel();
    sender
        .send((group, guard, response, cleanup))
        .await
        .map_err(|e| e.to_string())?;
    received.await.map_err(|e| e.to_string())?
}

async fn remove_groups(
    handle: &Handle,
    groups: HashSet<u32>,
    names: Option<HashSet<String>>,
) -> Result<(), String> {
    if groups.len() == 1 {
        return delete_group(handle, *groups.iter().next().unwrap()).await;
    }
    let requests: Vec<_> = match names {
        Some(names) => names
            .into_iter()
            .map(|name| {
                LinkUnspec::new_with_name(&name)
                    .link_group(RETIRING_GROUP)
                    .build()
            })
            .collect(),
        None => handle
            .link()
            .get()
            .execute()
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|link| {
                link.attributes.iter().any(
                    |attr| matches!(attr, LinkAttribute::Group(group) if groups.contains(group)),
                )
            })
            .map(|link| {
                LinkUnspec::new_with_index(link.header.index)
                    .link_group(RETIRING_GROUP)
                    .build()
            })
            .collect(),
    };
    for request in requests {
        let result = handle.link().set(request).execute().await;
        if let Err(error) = result {
            if matches!(&error, rtnetlink::Error::NetlinkError(e)
                if e.code.map(std::num::NonZeroI32::get) == Some(-libc::ENODEV))
            {
                if groups.is_empty() {
                    continue;
                }
                // Deleting a veth also removes its peer; rediscover the remaining groups.
                return Box::pin(remove_groups(handle, groups, None)).await;
            }
            // Finish both assigned and unassigned groups on partial failure.
            let mut cleanup = delete_group(handle, RETIRING_GROUP).await;
            for group in &groups {
                if let Err(error) = delete_group(handle, *group).await {
                    cleanup = Err(error);
                }
            }
            return cleanup.map_err(|cleanup| format!("{error}; cleanup: {cleanup}"));
        }
    }
    delete_group(handle, RETIRING_GROUP).await
}

async fn delete_group(handle: &Handle, group: u32) -> Result<(), String> {
    let mut request = handle.link().del(0);
    request
        .message_mut()
        .attributes
        .push(LinkAttribute::Group(group));
    match request.execute().await {
        Ok(()) => Ok(()),
        Err(rtnetlink::Error::NetlinkError(e))
            if e.code.map(std::num::NonZeroI32::get) == Some(-libc::ENODEV) =>
        {
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires root and an isolated root network namespace"]
    async fn named_batches_and_partial_discovery_preserve_foreign_devices() {
        let (connection, handle, _) = rtnetlink::new_connection().unwrap();
        tokio::spawn(connection);
        for (name, group) in [("nnct_a", 110), ("nnct_b", 111), ("nnct_foreign", 42)] {
            handle
                .link()
                .add(rtnetlink::LinkDummy::new(name).link_group(group).build())
                .execute()
                .await
                .unwrap();
        }
        remove_groups(
            &handle,
            HashSet::from([110, 111]),
            Some(HashSet::from(["nnct_a".into(), "nnct_b".into()])),
        )
        .await
        .unwrap();
        let links: Vec<_> = handle.link().get().execute().try_collect().await.unwrap();
        assert!(!links.iter().any(|link| link.attributes.iter().any(
            |a| matches!(a, LinkAttribute::IfName(name) if name == "nnct_a" || name == "nnct_b")
        )));
        for (name, group) in [("nnct_a", 110), ("nnct_b", 111)] {
            handle
                .link()
                .add(rtnetlink::LinkDummy::new(name).link_group(group).build())
                .execute()
                .await
                .unwrap();
        }
        remove_groups(
            &handle,
            HashSet::from([110, 111]),
            Some(HashSet::from(["nnct_a".into(), "nnct_missing".into()])),
        )
        .await
        .unwrap();
        let links: Vec<_> = handle.link().get().execute().try_collect().await.unwrap();
        assert!(!links.iter().any(|link| link.attributes.iter().any(
            |a| matches!(a, LinkAttribute::IfName(name) if name == "nnct_a" || name == "nnct_b")
        )));
        let foreign = links
            .iter()
            .find(|link| {
                link.attributes
                    .iter()
                    .any(|a| matches!(a, LinkAttribute::IfName(name) if name == "nnct_foreign"))
            })
            .unwrap();
        assert!(foreign.attributes.contains(&LinkAttribute::Group(42)));
        handle
            .link()
            .del(foreign.header.index)
            .execute()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn cancelled_setup_releases_its_scheduling_hint() {
        let first = track_setup();
        let (started, received) = oneshot::channel();
        let setup = tokio::spawn(async move {
            let _guard = track_setup();
            started.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        received.await.unwrap();
        assert_eq!(ACTIVE_SETUPS.load(Ordering::Relaxed), 2);
        drop(first);
        assert_eq!(ACTIVE_SETUPS.load(Ordering::Relaxed), 1);
        setup.abort();
        assert!(setup.await.unwrap_err().is_cancelled());
        assert_eq!(ACTIVE_SETUPS.load(Ordering::Relaxed), 0);
    }
}
