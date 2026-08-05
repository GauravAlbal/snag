use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ulid::Ulid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Observation {
    pub schema_version: u32,
    pub observation_id: String,
    pub store_id: String,
    pub local_sequence: u64,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    
    pub created_at: String, // ISO8601 or similar

    pub source: SourceInfo,

    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind_assertion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity_assertion: Option<String>,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_behavior: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_behavior: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reproduction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workaround: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    
    pub sensitivity: Sensitivity,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<BTreeMap<String, String>>,
    
    pub context: ContextInfo,
    
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub artifacts: Vec<ArtifactReference>,

    /// Resolved repository IDs (primary + affected) persisted by the reporter.
    /// These live in the canonical payload so rebuild can reconstruct the
    /// `observation_repositories` projection without external state.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub affected_repository_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    Normal,
    Sensitive,
    Restricted,
}

impl Default for Sensitivity {
    fn default() -> Self {
        Sensitivity::Normal
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceInfo {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reporter_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_runtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detector_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detector_version: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ContextInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<RepositoryContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepositoryContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkout_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_common_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub git_remote_aliases: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relative_cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecutionContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pearl_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authority_sequence: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_invocation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_shape: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArtifactReference {
    pub digest: String,
    pub byte_length: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_name: Option<String>,
    pub created_at: String,
}

pub fn generate_id(prefix: &str) -> String {
    format!("{}_{}", prefix, Ulid::generate().to_string().to_lowercase())
}
