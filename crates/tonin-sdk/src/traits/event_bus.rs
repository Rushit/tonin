//! Event bus capability — at-least-once publish/subscribe with explicit ack.

use std::{
    collections::HashMap,
    pin::Pin,
    task::{Context as TaskContext, Poll},
    time::Duration,
};

use async_trait::async_trait;
use futures_core::Stream;

use crate::Error;

pub type MessageId = String;

#[async_trait]
pub trait EventBus: Send + Sync + 'static {
    /// Publish bytes to a subject. The instrumented decorator injects
    /// `traceparent` into the headers it passes the impl, so subscribers
    /// in other services can continue the trace.
    async fn publish(&self, subject: &str, payload: &[u8]) -> Result<MessageId, Error>;

    /// Same as `publish` with caller-supplied headers (e.g., idempotency
    /// key). The decorator still adds `traceparent` if not already set.
    async fn publish_with_headers(
        &self,
        subject: &str,
        payload: &[u8],
        headers: HashMap<String, String>,
    ) -> Result<MessageId, Error>;

    /// Subscribe to a subject pattern under a named consumer group.
    /// Multiple pods sharing the same group split work; distinct groups
    /// each receive every message.
    async fn subscribe(
        &self,
        subject_pattern: &str,
        group: &str,
        opts: SubscribeOptions,
    ) -> Result<Subscription, Error>;

    /// Span attribute `messaging.system`. `"redis" | "nats" | "kafka"`.
    fn system(&self) -> &'static str;
}

#[derive(Clone, Debug)]
pub struct SubscribeOptions {
    pub start: StartPosition,
    pub visibility_timeout: Duration,
    pub max_in_flight: usize,
}

impl Default for SubscribeOptions {
    fn default() -> Self {
        Self {
            start: StartPosition::Now,
            visibility_timeout: Duration::from_secs(30),
            max_in_flight: 32,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum StartPosition {
    Now,
    Earliest,
}

/// Stream of `DeliveredMessage` from a consumer group. Drop to
/// unsubscribe; the backend impl is expected to commit/checkpoint
/// cleanly on drop.
pub struct Subscription {
    inner: Pin<Box<dyn Stream<Item = DeliveredMessage> + Send>>,
}

impl Subscription {
    pub fn new(s: impl Stream<Item = DeliveredMessage> + Send + 'static) -> Self {
        Self { inner: Box::pin(s) }
    }
}

impl Stream for Subscription {
    type Item = DeliveredMessage;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

/// A message delivered to a subscriber. `ack` and `nack` consume `self`
/// so double-ack is a compile error.
pub struct DeliveredMessage {
    pub id: MessageId,
    pub subject: String,
    pub payload: Vec<u8>,
    pub headers: HashMap<String, String>,
    pub delivery_attempt: u32,
    acker: Box<dyn Acker>,
}

impl DeliveredMessage {
    pub fn new(
        id: MessageId,
        subject: String,
        payload: Vec<u8>,
        headers: HashMap<String, String>,
        delivery_attempt: u32,
        acker: impl Acker + 'static,
    ) -> Self {
        Self {
            id,
            subject,
            payload,
            headers,
            delivery_attempt,
            acker: Box::new(acker),
        }
    }

    pub async fn ack(self) -> Result<(), Error> {
        self.acker.ack().await
    }

    pub async fn nack(self) -> Result<(), Error> {
        self.acker.nack(None).await
    }

    pub async fn nack_with_delay(self, delay: Duration) -> Result<(), Error> {
        self.acker.nack(Some(delay)).await
    }
}

/// Backend-supplied per-message ack handle. Public so backend impl
/// crates outside this workspace can construct `DeliveredMessage`, but
/// service code never names this trait — the `ack/nack/nack_with_delay`
/// methods on `DeliveredMessage` are the only surface users see.
#[async_trait]
pub trait Acker: Send + Sync {
    async fn ack(self: Box<Self>) -> Result<(), Error>;
    async fn nack(self: Box<Self>, delay: Option<Duration>) -> Result<(), Error>;
}
