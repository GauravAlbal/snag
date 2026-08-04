use thiserror::Error;

#[derive(Error, Debug)]
pub enum SnagError {
    #[error("Store busy: could not acquire write lock")]
    StoreBusy,

    #[error("Idempotency conflict: different payload submitted with the same key")]
    IdempotencyConflict,

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Artifact error: {0}")]
    ArtifactError(String),
}
