//! Framework error type.
//!
//! One enum spans both transport errors (existing) and capability errors
//! (Phase 3). The capability variants carry a transient-vs-permanent
//! distinction so subscriber loops can implement retry/nack discipline
//! without matching on the variant directly — call `is_transient()`.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    // --- transport / config (pre-Phase-3) ---
    #[error("transport error: {0}")]
    Transport(#[from] tonic::transport::Error),

    #[error("config error: {0}")]
    Config(String),

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),

    // --- capability errors (Phase 3) ---
    /// The backend rejected the call for a reason that won't go away with
    /// a retry: auth failure, malformed key, schema mismatch. Fix the call.
    #[error("capability call rejected: {0}")]
    CapabilityPermanent(String),

    /// Transient backend trouble: connection blip, timeout, throttle.
    /// Safe to retry or nack-with-backoff.
    #[error("capability call failed transiently: {0}")]
    CapabilityTransient(String),

    /// The impl is a deliberate stub. `tonin-redis::EventBus` returns
    /// this in Phase 4 (Q2). Callers can detect it and fail loud rather
    /// than silently dropping work.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

impl Error {
    /// True for backend failures that may succeed on retry (network blip,
    /// timeout, throttle). Subscriber loops use this to decide between
    /// nack-with-backoff and ack-and-drop.
    pub fn is_transient(&self) -> bool {
        matches!(self, Error::CapabilityTransient(_))
    }
}

impl From<crate::auth::AuthError> for Error {
    fn from(e: crate::auth::AuthError) -> Self {
        Error::Config(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
