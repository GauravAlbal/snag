use crate::types::{ContextInfo, ExecutionContext, RepositoryContext, SourceInfo};
use crate::git::collect_git_context;
use crate::cli::ReportArgs;
use std::env;
use std::path::PathBuf;
use anyhow::Result;

fn build_repo_context(args: &ReportArgs, git_ctx: &crate::git::GitContext) -> RepositoryContext {
    let mut repo_ctx = RepositoryContext {
        repository_id: env::var("VX_REPOSITORY_ID").ok(),
        checkout_id: env::var("VX_CHECKOUT_ID").ok(),
        worktree_id: env::var("VX_WORKTREE_ID").ok(),
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
    let mut exec_ctx = ExecutionContext {
        cwd: Some(cwd.to_string_lossy().to_string()),
        workspace_id: env::var("ARQ_WORKSPACE_ID").ok(),
        program_id: env::var("ARQ_PROGRAM_ID").ok(),
        session_id: env::var("ARQ_SESSION_ID").ok(),
        pearl_id: env::var("VX_PEARL_ID").ok(),
        attempt_id: env::var("VX_ATTEMPT_ID").ok(),
        authority_sequence: env::var("VX_AUTHORITY_SEQUENCE").ok().and_then(|v| v.parse().ok()),
        tool_name: None,
        tool_invocation_id: None,
        command_shape: None,
    };

    if let Some(ref sid) = args.session_id {
        exec_ctx.session_id = Some(sid.clone());
    }
    if let Some(ref pid) = args.pearl_id {
        exec_ctx.pearl_id = Some(pid.clone());
    }
    if let Some(ref aid) = args.attempt_id {
        exec_ctx.attempt_id = Some(aid.clone());
    }
    exec_ctx
}

pub fn gather_context(args: &ReportArgs) -> Result<(SourceInfo, ContextInfo)> {
    let mut source = SourceInfo {
        kind: env::var("SNAG_SOURCE_KIND").unwrap_or_else(|_| "human_explicit".to_string()),
        system: None,
        reporter_id: env::var("SNAG_REPORTER_ID").ok(),
        agent_runtime: None,
        agent_name: None,
        model: None,
        detector_id: None,
        detector_version: None,
    };

    let cwd = env::current_dir()?;
    let git_ctx = collect_git_context(&cwd).unwrap_or_default();

    let repo_ctx = build_repo_context(args, &git_ctx);
    let exec_ctx = build_exec_context(args, &cwd);

    // Attempt to load context file if SNAG_CONTEXT_FILE is provided
    if let Ok(ctx_file) = env::var("SNAG_CONTEXT_FILE") {
        if let Ok(content) = std::fs::read_to_string(ctx_file) {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(src) = parsed.get("source") {
                    if let Ok(s) = serde_json::from_value(src.clone()) {
                        source = s;
                    }
                }
            }
        }
    }

    let ctx_info = ContextInfo {
        repository: Some(repo_ctx),
        execution: Some(exec_ctx),
        extra: None,
    };

    Ok((source, ctx_info))
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
        session_id: None,
        pearl_id: None,
        attempt_id: None,
        affected_repos: vec![],
    };
    
    let (_, ctx) = gather_context(&dummy_args)?;
    
    if args.format.as_deref() == Some("json") {
        println!("{}", serde_json::to_string_pretty(&ctx)?);
    } else {
        println!("{:#?}", ctx);
    }
    
    Ok(())
}
