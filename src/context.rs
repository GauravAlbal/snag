use crate::cli::ReportArgs;
use crate::error::SnagError;
use crate::git::collect_git_context;
use crate::types::{ContextInfo, ExecutionContext, RepositoryContext, SourceInfo};
use anyhow::Result;
use serde::Deserialize;
use std::env;

fn build_repo_context(args: &ReportArgs, git_ctx: &crate::git::GitContext) -> RepositoryContext {
    let mut repo_ctx = RepositoryContext {
        repository_id: None,
        checkout_id: None,
        worktree_id: None,
        repository_root: git_ctx.repository_root.clone(),
        git_common_dir: git_ctx.git_common_dir.clone(),
        git_head: git_ctx.git_head.clone(),
        git_branch: git_ctx.git_branch.clone(),
        git_remote_aliases: git_ctx.git_remote_aliases.clone(),
        relative_cwd: git_ctx.relative_cwd.clone(),
    };

    if let Some(ref rid) = args.repo_id {
        repo_ctx.repository_id = Some(rid.clone());
    }
    repo_ctx
}

fn build_exec_context(args: &ReportArgs, cwd: &std::path::Path) -> ExecutionContext {
    // Session/work-item identity flows through the CLI (`--session-id`,
    // `--task-id`), the context file, or the reporter; no internal env
    // wiring leaks into the public surface.
    let mut exec_ctx = ExecutionContext {
        cwd: Some(cwd.to_string_lossy().to_string()),
        workspace_id: None,
        program_id: None,
        session_id: None,
        task_id: None,
        attempt_id: None,
        authority_sequence: None,
        tool_name: None,
        tool_invocation_id: None,
        command_shape: None,
    };

    if let Some(ref sid) = args.session_id {
        exec_ctx.session_id = Some(sid.clone());
    }
    if let Some(ref tid) = args.task_id {
        exec_ctx.task_id = Some(tid.clone());
    }
    if let Some(ref aid) = args.attempt_id {
        exec_ctx.attempt_id = Some(aid.clone());
    }
    exec_ctx
}

/// Shape of the optional `SNAG_CONTEXT_FILE` document.
#[derive(Deserialize)]
struct ContextFile {
    /// Protocol version of the context document. Must be 1 (or absent for
    /// backward-compatible writers); a future major version is rejected.
    schema_version: Option<u32>,
    source: Option<SourceInfo>,
    execution: Option<ExecutionContext>,
    repository: Option<RepositoryContext>,
    extra: Option<serde_json::Value>,
    idempotency_key: Option<String>,
}

/// Baseline source identity taken from the environment.
fn build_source() -> SourceInfo {
    SourceInfo {
        kind: env::var("SNAG_SOURCE_KIND").unwrap_or_else(|_| "human_explicit".to_string()),
        system: None,
        reporter_id: env::var("SNAG_REPORTER_ID").ok(),
        agent_runtime: None,
        agent_name: None,
        model: None,
        detector_id: None,
        detector_version: None,
    }
}

/// Overlay repository fields present in the context file.
/// Overlay a context-file source over the environment-derived base, field by
/// field (only present fields replace). Matches the repository/execution
/// merge semantics — a partial context file must not wipe fields the
/// environment already established (e.g. `SNAG_SOURCE_KIND`).
fn merge_source_context(base: &mut SourceInfo, src: SourceInfo) {
    // `kind` is a required SourceInfo field: when the context file names a
    // source object it always carries a kind, so it always overlays.
    base.kind = src.kind;
    if let Some(s) = src.system {
        base.system = Some(s);
    }
    if let Some(r) = src.reporter_id {
        base.reporter_id = Some(r);
    }
    if let Some(a) = src.agent_runtime {
        base.agent_runtime = Some(a);
    }
    if let Some(a) = src.agent_name {
        base.agent_name = Some(a);
    }
    if let Some(m) = src.model {
        base.model = Some(m);
    }
    if let Some(d) = src.detector_id {
        base.detector_id = Some(d);
    }
    if let Some(d) = src.detector_version {
        base.detector_version = Some(d);
    }
}

fn merge_repo_context(repo_ctx: &mut RepositoryContext, repo: RepositoryContext) {
    if let Some(rid) = repo.repository_id {
        repo_ctx.repository_id = Some(rid);
    }
    if let Some(cid) = repo.checkout_id {
        repo_ctx.checkout_id = Some(cid);
    }
    if let Some(wid) = repo.worktree_id {
        repo_ctx.worktree_id = Some(wid);
    }
}

/// Overlay execution fields present in the context file.
fn merge_exec_context(exec_ctx: &mut ExecutionContext, exec: ExecutionContext) {
    if let Some(wid) = exec.workspace_id {
        exec_ctx.workspace_id = Some(wid);
    }
    if let Some(pid) = exec.program_id {
        exec_ctx.program_id = Some(pid);
    }
    if let Some(sid) = exec.session_id {
        exec_ctx.session_id = Some(sid);
    }
    if let Some(tid) = exec.task_id {
        exec_ctx.task_id = Some(tid);
    }
    if let Some(aid) = exec.attempt_id {
        exec_ctx.attempt_id = Some(aid);
    }
    if let Some(auth) = exec.authority_sequence {
        exec_ctx.authority_sequence = Some(auth);
    }
    if let Some(tn) = exec.tool_name {
        exec_ctx.tool_name = Some(tn);
    }
    if let Some(ti) = exec.tool_invocation_id {
        exec_ctx.tool_invocation_id = Some(ti);
    }
    if let Some(cs) = exec.command_shape {
        exec_ctx.command_shape = Some(cs);
    }
}

/// Read and overlay the context file at `path`. The context file overrides the
/// environment; explicit CLI arguments override the context file.
fn merge_context_file(
    path: &str,
    source: &mut SourceInfo,
    repo_ctx: &mut RepositoryContext,
    exec_ctx: &mut ExecutionContext,
    extra: &mut Option<serde_json::Value>,
    idempotency_key: &mut Option<String>,
) -> Result<()> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        SnagError::ContextFileInvalid(format!("Could not read context file: {}", e))
    })?;
    let parsed: ContextFile = serde_json::from_str(&content)
        .map_err(|e| SnagError::ContextFileInvalid(format!("Invalid context file JSON: {}", e)))?;

    if let Some(sv) = parsed.schema_version
        && sv != 1
    {
        return Err(SnagError::UnsupportedSchema(sv.to_string()).into());
    }

    if let Some(src) = parsed.source {
        // Context file overlays individual source fields (matching the
        // repository/execution merge behavior) instead of replacing the whole
        // struct: SNAG_SOURCE_KIND from the environment survives unless the
        // context file names its own kind.
        merge_source_context(source, src);
    }
    if let Some(repo) = parsed.repository {
        merge_repo_context(repo_ctx, repo);
    }
    if let Some(exec) = parsed.execution {
        merge_exec_context(exec_ctx, exec);
    }
    if let Some(ext) = parsed.extra {
        *extra = Some(ext);
    }
    if let Some(ik) = parsed.idempotency_key
        && idempotency_key.is_none()
    {
        *idempotency_key = Some(ik);
    }
    Ok(())
}

/// Explicit CLI arguments take precedence over the context file.
fn apply_overrides(
    args: &ReportArgs,
    repo_ctx: &mut RepositoryContext,
    exec_ctx: &mut ExecutionContext,
) {
    if let Some(rid) = &args.repo_id {
        repo_ctx.repository_id = Some(rid.clone());
    }
    if let Some(sid) = &args.session_id {
        exec_ctx.session_id = Some(sid.clone());
    }
    if let Some(tid) = &args.task_id {
        exec_ctx.task_id = Some(tid.clone());
    }
    if let Some(aid) = &args.attempt_id {
        exec_ctx.attempt_id = Some(aid.clone());
    }
}

pub fn gather_context(args: &ReportArgs) -> Result<(SourceInfo, ContextInfo, Option<String>)> {
    let mut source = build_source();

    let cwd = env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let git_ctx = collect_git_context(&cwd).unwrap_or_default();

    let mut repo_ctx = build_repo_context(args, &git_ctx);
    let mut exec_ctx = build_exec_context(args, &cwd);

    let mut extra = None;
    let mut idempotency_key = args.idempotency_key.clone();

    // Attempt to load context file if SNAG_CONTEXT_FILE is provided
    if let Ok(ctx_file) = env::var("SNAG_CONTEXT_FILE") {
        merge_context_file(
            &ctx_file,
            &mut source,
            &mut repo_ctx,
            &mut exec_ctx,
            &mut extra,
            &mut idempotency_key,
        )?;
    }

    // Explicit CLI arguments override context file
    apply_overrides(args, &mut repo_ctx, &mut exec_ctx);

    let ctx_info = ContextInfo {
        repository: Some(repo_ctx),
        execution: Some(exec_ctx),
        extra,
    };

    Ok((source, ctx_info, idempotency_key))
}

pub fn handle(args: crate::cli::ContextArgs) -> anyhow::Result<()> {
    let dummy_args = ReportArgs {
        title: None,
        kind: None,
        severity: None,
        expected: None,
        observed: None,
        workaround: None,
        repro: None,
        json: false,
        stdin: false,
        artifacts: vec![],
        idempotency_key: None,
        repo_id: None,
        owner: None,
        session_id: None,
        task_id: None,
        attempt_id: None,
        affected_repos: vec![],
    };

    let (_, ctx, _) = gather_context(&dummy_args)?;

    if args.format.as_deref() == Some("json") {
        // Versioned envelope: consumers must key on `schema_version`.
        let envelope = serde_json::json!({ "schema_version": 1, "context": ctx });
        println!("{}", serde_json::to_string_pretty(&envelope)?);
    } else {
        println!("{:#?}", ctx);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_source_merges_field_by_field() {
        // Environment-derived base: kind from SNAG_SOURCE_KIND.
        let mut base = SourceInfo {
            kind: "agent_explicit".to_string(),
            system: None,
            reporter_id: Some("env_reporter".to_string()),
            agent_runtime: None,
            agent_name: None,
            model: None,
            detector_id: None,
            detector_version: None,
        };
        // A partial context-file source: only kind + reporter_id present.
        let partial = SourceInfo {
            kind: "agent_report".to_string(),
            system: None,
            reporter_id: None,
            agent_runtime: Some("test_runtime".to_string()),
            agent_name: None,
            model: None,
            detector_id: None,
            detector_version: None,
        };
        merge_source_context(&mut base, partial);
        // Present fields overlay; absent fields keep the base.
        assert_eq!(base.kind, "agent_report");
        assert_eq!(base.reporter_id.as_deref(), Some("env_reporter"));
        assert_eq!(base.agent_runtime.as_deref(), Some("test_runtime"));
        // Unrelated base fields survive.
        assert_eq!(base.system, None);
    }
}
