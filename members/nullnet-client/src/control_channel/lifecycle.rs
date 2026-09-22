use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

type Tail = (Arc<()>, oneshot::Receiver<()>);

#[derive(Default)]
pub(super) struct LifecycleTasks {
    tails: Arc<Mutex<HashMap<u32, Tail>>>,
}

impl LifecycleTasks {
    // Enqueue before spawning so scheduling cannot reorder a tunnel's commands.
    pub(super) fn spawn(&self, id: u32, work: impl Future<Output = ()> + Send + 'static) {
        let token = Arc::new(());
        let (done, finished) = oneshot::channel();
        let previous = self
            .tails
            .lock()
            .unwrap()
            .insert(id, (token.clone(), finished));
        let tails = self.tails.clone();
        tokio::spawn(async move {
            if let Some((_, previous)) = previous {
                let _ = previous.await;
            }
            work.await;
            let mut tails = tails.lock().unwrap();
            if tails
                .get(&id)
                .is_some_and(|(tail, _)| Arc::ptr_eq(tail, &token))
            {
                tails.remove(&id);
            }
            let _ = done.send(());
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn orders_one_tunnel_without_blocking_another_and_releases_state() {
        let tasks = LifecycleTasks::default();
        let (release, held) = oneshot::channel();
        let (events, mut observed) = tokio::sync::mpsc::unbounded_channel();
        let first = events.clone();
        tasks.spawn(1, async move {
            held.await.unwrap();
            first.send("setup").unwrap();
        });
        let second = events.clone();
        tasks.spawn(1, async move {
            second.send("ready").unwrap();
        });
        let third = events.clone();
        tasks.spawn(1, async move {
            third.send("teardown").unwrap();
        });
        tasks.spawn(2, async move {
            events.send("other").unwrap();
        });
        assert_eq!(observed.recv().await, Some("other"));
        assert!(observed.try_recv().is_err());
        release.send(()).unwrap();
        for expected in ["setup", "ready", "teardown"] {
            assert_eq!(observed.recv().await, Some(expected));
        }
        assert!(tasks.tails.lock().unwrap().is_empty());
    }
}
