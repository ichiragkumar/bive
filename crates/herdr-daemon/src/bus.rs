//! Global event bus: broadcast channel connecting agent tasks to every client.

use tokio::sync::broadcast;

use herdr_protocol::DaemonEvent;

const BUS_CAPACITY: usize = 1024;

/// Wrapper over `tokio::sync::broadcast` with the daemon's lag policy:
/// subscribers that stop reading are dropped, never allowed to stall the daemon.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<DaemonEvent>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(BUS_CAPACITY);
        Self { tx }
    }

    /// Publish an event to all subscribers. Returns the number of receivers —
    /// 0 is normal when no client is attached (daemon still persists state).
    pub fn publish(&self, event: DaemonEvent) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    /// Subscribe a new consumer (one per IPC client connection).
    pub fn subscribe(&self) -> broadcast::Receiver<DaemonEvent> {
        self.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use herdr_protocol::{AgentState, DaemonEvent};

    #[tokio::test]
    async fn publish_reaches_subscribers() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        bus.publish(DaemonEvent::StateChange {
            agent_id: "a".into(),
            state: AgentState::Working,
        });
        let got = rx.recv().await.unwrap();
        assert!(matches!(got, DaemonEvent::StateChange { .. }));
    }

    #[tokio::test]
    async fn publish_with_no_subscribers_is_fine() {
        let bus = EventBus::new();
        assert_eq!(
            bus.publish(DaemonEvent::AgentRemoved {
                agent_id: "x".into()
            }),
            0
        );
    }

    #[tokio::test]
    async fn lagged_subscriber_sees_lagged_error() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        for i in 0..(1024 + 10) {
            bus.publish(DaemonEvent::AgentOutput {
                agent_id: "a".into(),
                payload: i.to_string(),
            });
        }
        assert!(matches!(
            rx.recv().await,
            Err(broadcast::error::RecvError::Lagged(_))
        ));
    }
}
