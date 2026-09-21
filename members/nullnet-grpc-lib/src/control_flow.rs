//! Bound unread control frames independently of operation completion.
use crate::nullnet_grpc::{MsgId, NetMessage};
use std::collections::HashSet;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use tokio::sync::mpsc;
use tonic::codegen::tokio_stream::Stream;

// Eight messages plus eight receipts leave space for the 32 unary RPCs.
const WINDOW: usize = 8;

#[doc(hidden)]
pub trait ControlFrame {
    fn set_sequence(&mut self, sequence: u64);
    fn receipt(sequence: u64) -> Self;
}

macro_rules! frame {
    ($kind:ty) => {
        impl ControlFrame for $kind {
            fn set_sequence(&mut self, sequence: u64) {
                self.delivery_sequence = sequence;
            }
            fn receipt(sequence: u64) -> Self {
                Self {
                    delivery_receipt: sequence,
                    ..Self::default()
                }
            }
        }
    };
}
frame!(MsgId);
frame!(NetMessage);

impl<T: ControlFrame> ControlFrame for Result<T, tonic::Status> {
    fn set_sequence(&mut self, sequence: u64) {
        if let Ok(frame) = self {
            frame.set_sequence(sequence);
        }
    }
    fn receipt(sequence: u64) -> Self {
        Ok(T::receipt(sequence))
    }
}

#[derive(Default)]
struct Pending {
    sequences: HashSet<u64>,
    waker: Option<Waker>,
}

/// Per-connection receipt state; receipts never acknowledge kernel work.
#[derive(Clone)]
pub struct ControlFlow {
    pending: Arc<Mutex<Pending>>,
    receipts: mpsc::Sender<u64>,
}

impl ControlFlow {
    /// Returns true for a delivered application message, false for a receipt.
    pub async fn receive(&self, sequence: u64, receipt: u64) -> bool {
        let waker = {
            let mut pending = self.pending.lock().unwrap();
            if pending.sequences.remove(&receipt) {
                pending.waker.take()
            } else {
                None
            }
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        sequence != 0 && self.receipts.send(sequence).await.is_ok()
    }
}

/// A bounded stream with priority receipts, so two full windows cannot deadlock.
pub struct ControlStream<T> {
    messages: mpsc::Receiver<T>,
    receipts: mpsc::Receiver<u64>,
    pending: Arc<Mutex<Pending>>,
    next_sequence: u64,
}

impl<T> ControlStream<T> {
    pub fn new(messages: mpsc::Receiver<T>) -> (Self, ControlFlow) {
        let (tx, receipts) = mpsc::channel(WINDOW);
        let pending = Arc::new(Mutex::new(Pending::default()));
        (
            Self {
                messages,
                receipts,
                pending: pending.clone(),
                next_sequence: 1,
            },
            ControlFlow {
                pending,
                receipts: tx,
            },
        )
    }
}

impl<T: ControlFrame + Unpin> Stream for ControlStream<T> {
    type Item = T;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        if let Poll::Ready(Some(sequence)) = self.receipts.poll_recv(cx) {
            return Poll::Ready(Some(T::receipt(sequence)));
        }
        if self.messages.is_closed() && self.messages.is_empty() {
            return Poll::Ready(None);
        }
        {
            let mut pending = self.pending.lock().unwrap();
            if pending.sequences.len() == WINDOW {
                pending.waker = Some(cx.waker().clone());
                return Poll::Pending;
            }
        }
        let Poll::Ready(message) = self.messages.poll_recv(cx) else {
            return Poll::Pending;
        };
        let Some(mut message) = message else {
            return Poll::Ready(None);
        };
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        message.set_sequence(sequence);
        self.pending.lock().unwrap().sequences.insert(sequence);
        Poll::Ready(Some(message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonic::codegen::tokio_stream::StreamExt;

    #[tokio::test]
    async fn full_windows_exchange_receipts_without_completing_operations() {
        let (a_tx, a_rx) = mpsc::channel(64);
        let (b_tx, b_rx) = mpsc::channel(64);
        let (mut a, a_flow) = ControlStream::<MsgId>::new(a_rx);
        let (mut b, b_flow) = ControlStream::<NetMessage>::new(b_rx);
        for _ in 0..WINDOW + 1 {
            a_tx.send(MsgId::default()).await.unwrap();
            b_tx.send(NetMessage::default()).await.unwrap();
        }
        for _ in 0..WINDOW {
            let a_message = a.next().await.unwrap();
            let b_message = b.next().await.unwrap();
            assert!(b_flow.receive(a_message.delivery_sequence, 0).await);
            assert!(a_flow.receive(b_message.delivery_sequence, 0).await);
            // Drain only the priority receipts, withholding their delivery.
            assert_ne!(a.next().await.unwrap().delivery_receipt, 0);
            assert_ne!(b.next().await.unwrap().delivery_receipt, 0);
        }
        assert_eq!(a.pending.lock().unwrap().sequences.len(), WINDOW);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), a.next())
                .await
                .is_err()
        );
        assert!(!a_flow.receive(0, 1).await);
        assert!(!a_flow.receive(0, 1).await); // Duplicate receipts grant no extra credit.
        assert_eq!(
            a.next().await.unwrap().delivery_sequence,
            (WINDOW + 1) as u64
        );
        assert_eq!(a.pending.lock().unwrap().sequences.len(), WINDOW);
        // Receipt traffic still progresses while the data window is full.
        assert!(a_flow.receive(99, 0).await);
        assert_eq!(a.next().await.unwrap().delivery_receipt, 99);
        drop(a_tx);
        assert!(a.next().await.is_none());
    }
    #[tokio::test]
    async fn sustained_bidirectional_delivery_preserves_every_completion_id() {
        let (a_tx, a_rx) = mpsc::channel(64);
        let (b_tx, b_rx) = mpsc::channel(64);
        let (mut a, a_flow) = ControlStream::<MsgId>::new(a_rx);
        let (mut b, b_flow) = ControlStream::<MsgId>::new(b_rx);
        let producer = tokio::spawn(async move {
            for id in 0..10_000 {
                a_tx.send(MsgId {
                    id: id.to_string(),
                    ..Default::default()
                })
                .await
                .unwrap();
                b_tx.send(MsgId {
                    id: id.to_string(),
                    ..Default::default()
                })
                .await
                .unwrap();
            }
            (a_tx, b_tx)
        });
        let transfer = async {
            let (mut received_a, mut received_b) = (0, 0);
            while received_a < 10_000 || received_b < 10_000 {
                tokio::select! {
                    Some(message) = a.next() => {
                        if b_flow.receive(message.delivery_sequence, message.delivery_receipt).await {
                            assert_eq!(message.id, received_b.to_string());
                            received_b += 1;
                        }
                    }
                    Some(message) = b.next() => {
                        if a_flow.receive(message.delivery_sequence, message.delivery_receipt).await {
                            assert_eq!(message.id, received_a.to_string());
                            received_a += 1;
                        }
                    }
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), transfer)
            .await
            .unwrap();
        drop(producer.await.unwrap());
    }
}
