use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("session closed")]
    Closed,
    #[error("queue full (backpressure)")]
    QueueFull,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
