use futures::TryStreamExt;
use rtnetlink::packet_route::link::LinkAttribute;
use rtnetlink::{Handle, LinkUnspec};
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};
use tokio::sync::{OwnedMutexGuard, mpsc, oneshot};

// A single worker owns this temporary group; only retiring links enter it.
const RETIRING_GROUP: u32 = 0x4e40_0000;
type Request = (
    u32,
    Arc<OwnedMutexGuard<()>>,
    oneshot::Sender<Result<(), String>>,
);
static CLEANUP: OnceLock<mpsc::Sender<Request>> = OnceLock::new();

pub(super) async fn remove(
    handle: &Handle,
    group: u32,
    guard: Arc<OwnedMutexGuard<()>>,
) -> Result<(), String> {
    let sender = CLEANUP.get_or_init(|| {
        let (sender, mut receiver) = mpsc::channel::<Request>(1024);
        let handle = handle.clone();
        tokio::spawn(async move {
            let mut batch = Vec::with_capacity(128);
            while receiver.recv_many(&mut batch, 128).await != 0 {
                let groups = batch.iter().map(|(group, _, _)| *group).collect();
                let result = remove_groups(&handle, groups).await;
                for (_, _guard, response) in batch.drain(..) {
                    let _ = response.send(result.clone());
                }
            }
        });
        sender
    });
    let (response, received) = oneshot::channel();
    sender
        .send((group, guard, response))
        .await
        .map_err(|e| e.to_string())?;
    received.await.map_err(|e| e.to_string())?
}

async fn remove_groups(handle: &Handle, groups: HashSet<u32>) -> Result<(), String> {
    if groups.len() == 1 {
        return delete_group(handle, *groups.iter().next().unwrap()).await;
    }
    let links: Vec<_> = handle
        .link()
        .get()
        .execute()
        .try_collect()
        .await
        .map_err(|e| e.to_string())?;
    for link in links {
        if link
            .attributes
            .iter()
            .any(|attr| matches!(attr, LinkAttribute::Group(group) if groups.contains(group)))
        {
            let result = handle
                .link()
                .set(
                    LinkUnspec::new_with_index(link.header.index)
                        .link_group(RETIRING_GROUP)
                        .build(),
                )
                .execute()
                .await;
            if let Err(error) = result {
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
