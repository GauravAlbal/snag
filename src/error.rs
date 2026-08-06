use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Serialize)]
pub struct ErrorEnvelope {
    pub schema_version: u32,
    pub error: ErrorDetail,
}

#[derive(Debug, Serialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

#[derive(Error, Debug)]
pub enum SnagError {
    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Unsupported schema: {0}")]
    UnsupportedSchema(String),

    #[error("Idempotency conflict: {0}")]
    IdempotencyConflict(String),

    #[error("Store busy: {0}")]
    StoreBusy(String),

    #[error("Store corrupt: {0}")]
    StoreCorrupt(String),

    #[error("Context file invalid: {0}")]
    ContextFileInvalid(String),

    #[error("Repository ambiguous: {0}")]
    RepositoryAmbiguous(String),

    #[error("Repository not found: {0}")]
    RepositoryNotFound(String),

    #[error("Repository invalid: {0}")]
    RepositoryInvalid(String),

    #[error("Artifact too large: {0}")]
    ArtifactTooLarge(String),

    #[error("Artifact invalid: {0}")]
    ArtifactInvalid(String),

    #[error("Backup invalid: {0}")]
    BackupInvalid(String),

    #[error("Restore refused: {0}")]
    RestoreRefused(String),

    #[error("Export invalid: {0}")]
    ExportInvalid(String),

    #[error("Claim conflict: {0}")]
    ClaimConflict(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl SnagError {
    pub fn to_envelope(&self) -> ErrorEnvelope {
        let code = match self {
            Self::Validation(_) => "VALIDATION_ERROR",
            Self::UnsupportedSchema(_) => "UNSUPPORTED_SCHEMA",
            Self::IdempotencyConflict(_) => "IDEMPOTENCY_CONFLICT",
            Self::StoreBusy(_) => "STORE_BUSY",
            Self::StoreCorrupt(_) => "STORE_CORRUPT",
            Self::ContextFileInvalid(_) => "CONTEXT_FILE_INVALID",
            Self::RepositoryAmbiguous(_) => "REPOSITORY_AMBIGUOUS",
            Self::RepositoryNotFound(_) => "REPOSITORY_NOT_FOUND",
            Self::RepositoryInvalid(_) => "REPOSITORY_INVALID",
            Self::ArtifactTooLarge(_) => "ARTIFACT_TOO_LARGE",
            Self::ArtifactInvalid(_) => "ARTIFACT_INVALID",
            Self::BackupInvalid(_) => "BACKUP_INVALID",
            Self::RestoreRefused(_) => "RESTORE_REFUSED",
            Self::ExportInvalid(_) => "EXPORT_INVALID",
            Self::ClaimConflict(_) => "CLAIM_CONFLICT",
            Self::NotFound(_) => "NOT_FOUND",
            Self::Other(_) => "INTERNAL_ERROR",
        };

        ErrorEnvelope {
            schema_version: 1,
            error: ErrorDetail {
                code: code.to_string(),
                message: self.to_string(),
                details: None,
            },
        }
    }
}
