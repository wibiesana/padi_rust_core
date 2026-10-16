//! In-Memory Realtime Hub & Server-Sent Events (SSE) (mirrors Padi Go realtime architecture).
//!
//! Provides zero-infrastructure, high-performance in-memory pub/sub using Tokio broadcast channels
//! and Axum's native `Sse` response type.
//!
//! Features:
//! - `Realtime::publish(topic, data)`: Non-blocking broadcast to topic subscribers.
//! - `Realtime::publish_batch(events)`: Broadcast multiple events.
//! - `Realtime::broadcast(data)`: Broadcast to all active topics.
//! - `Realtime::subscriber_count(topic)`: Number of active listeners on a topic.
//! - `Realtime::topics()`: List of all active topics.
//! - `Realtime::sse_handler()`: Axum handler streaming SSE with query params `?topic=...` or `?topics=...`.
//! - Heartbeat ping / keepalive every 15 seconds.

use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{LazyLock, RwLock},
    time::Duration,
};

use axum::{
    extract::Query,
    response::sse::{Event, KeepAlive, Sse},
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_stream::{
    wrappers::{errors::BroadcastStreamRecvError, BroadcastStream},
    Stream, StreamExt,
};

/// Event message payload passed through topic channels.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RealtimeEvent {
    pub topic: String,
    pub data: Value,
}

struct Hub {
    channels: HashMap<String, broadcast::Sender<RealtimeEvent>>,
}

impl Hub {
    fn new() -> Self {
        Self {
            channels: HashMap::new(),
        }
    }

    fn get_or_create_sender(&mut self, topic: &str) -> broadcast::Sender<RealtimeEvent> {
        if let Some(sender) = self.channels.get(topic) {
            sender.clone()
        } else {
            let (tx, _rx) = broadcast::channel(128);
            self.channels.insert(topic.to_string(), tx.clone());
            tx
        }
    }

    fn cleanup_inactive(&mut self) {
        self.channels
            .retain(|_, tx| tx.receiver_count() > 0);
    }
}

static GLOBAL_HUB: LazyLock<RwLock<Hub>> =
    LazyLock::new(|| RwLock::new(Hub::new()));

pub struct Realtime;

impl Realtime {
    /// Publish an event to all subscribers listening to `topic`.
    ///
    /// # Example
    /// ```rust,no_run
    /// use padi_core::realtime::Realtime;
    /// use serde_json::json;
    ///
    /// Realtime::publish("orders", json!({ "id": 42, "status": "paid" }));
    /// ```
    pub fn publish<V: Into<Value>>(topic: &str, data: V) {
        let event = RealtimeEvent {
            topic: topic.to_string(),
            data: data.into(),
        };

        if let Ok(mut hub) = GLOBAL_HUB.write() {
            hub.cleanup_inactive();
            let sender = hub.get_or_create_sender(topic);
            let _ = sender.send(event);
        }
    }

    /// Publish multiple events at once.
    pub fn publish_batch<V: Into<Value>>(events: Vec<(&str, V)>) {
        for (topic, data) in events {
            Self::publish(topic, data);
        }
    }

    /// Broadcast data to all currently registered topics.
    pub fn broadcast<V: Into<Value>>(data: V) {
        let data_val = data.into();
        let topics = Self::topics();
        for topic in topics {
            Self::publish(&topic, data_val.clone());
        }
    }

    /// Return the number of active subscribers on `topic`.
    pub fn subscriber_count(topic: &str) -> usize {
        if let Ok(hub) = GLOBAL_HUB.read() {
            if let Some(tx) = hub.channels.get(topic) {
                return tx.receiver_count();
            }
        }
        0
    }

    /// Return a list of all active topics with at least one subscriber.
    pub fn topics() -> Vec<String> {
        if let Ok(mut hub) = GLOBAL_HUB.write() {
            hub.cleanup_inactive();
            return hub
                .channels
                .iter()
                .filter(|(_, tx)| tx.receiver_count() > 0)
                .map(|(k, _)| k.clone())
                .collect();
        }
        Vec::new()
    }

    /// Subscribe to one or more topics, returning an async Stream of SSE Events.
    pub fn subscribe(topics: &[&str]) -> impl Stream<Item = Result<Event, Infallible>> {
        let mut rx_streams = Vec::new();

        if let Ok(mut hub) = GLOBAL_HUB.write() {
            hub.cleanup_inactive();
            for topic in topics {
                let tx = hub.get_or_create_sender(topic);
                let rx = tx.subscribe();
                rx_streams.push(BroadcastStream::new(rx));
            }
        }

        // Merge streams into one
        let merged = tokio_stream::StreamMap::from_iter(
            rx_streams.into_iter().enumerate()
        );

        merged.filter_map(|(_idx, res)| match res {
            Ok(event) => {
                let payload = serde_json::to_string(&event.data).unwrap_or_default();
                Some(Ok(Event::default().event(event.topic).data(payload)))
            }
            Err(BroadcastStreamRecvError::Lagged(_)) => None,
        })
    }

    /// Axum HTTP handler for SSE clients.
    ///
    /// Reads query params:
    /// - `?topic=my-channel`
    /// - `?topics=channel1,channel2`
    pub fn sse_handler(
        Query(params): Query<SseParams>,
    ) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
        let mut topic_list = Vec::new();

        if let Some(topics_csv) = params.topics {
            for t in topics_csv.split(',') {
                let trimmed = t.trim();
                if !trimmed.is_empty() {
                    topic_list.push(trimmed.to_string());
                }
            }
        } else if let Some(single_topic) = params.topic {
            let trimmed = single_topic.trim();
            if !trimmed.is_empty() {
                topic_list.push(trimmed.to_string());
            }
        }

        let topic_refs: Vec<&str> = topic_list.iter().map(|s| s.as_str()).collect();
        let stream = Self::subscribe(&topic_refs);

        Sse::new(stream).keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("ping"),
        )
    }
}

#[derive(Debug, Deserialize)]
pub struct SseParams {
    pub topic: Option<String>,
    pub topics: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_realtime_pub_sub() {
        let mut stream = Realtime::subscribe(&["chat"]);
        assert_eq!(Realtime::subscriber_count("chat"), 1);

        Realtime::publish("chat", json!({ "msg": "hello world" }));

        if let Some(Ok(event)) = stream.next().await {
            // Event received successfully
            assert!(format!("{:?}", event).contains("hello world"));
        } else {
            panic!("Expected event from stream");
        }
    }

    #[test]
    fn test_topics_and_counts() {
        let _stream = Realtime::subscribe(&["orders", "alerts"]);
        let topics = Realtime::topics();
        assert!(topics.contains(&"orders".to_string()));
        assert!(topics.contains(&"alerts".to_string()));
        assert_eq!(Realtime::subscriber_count("orders"), 1);
        assert_eq!(Realtime::subscriber_count("unknown_topic"), 0);
    }
}
