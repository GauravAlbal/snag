use crate::cli::ReportArgs;
use crate::git::collect_git_context;
use crate::types::{ContextInfo, ExecutionContext, RepositoryContext, SourceInfo};
use anyhow::Result;
use std::env;

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
        authority_sequence: env::var("VX_AUTHORITY_SEQUENCE")
            .ok()
            .and_then(|v| v.parse().ok()),
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

pub fn gather_context(args: &ReportArgs) -> Result<(SourceInfo, ContextInfo, Option<String>)> {
    use crate::error::SnagError;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct ContextFile {
        source: Option<SourceInfo>,
        execution: Option<ExecutionContext>,
        repository: Option<RepositoryContext>,
        extra: Option<serde_json::Value>,
        idempotency_key: Option<String>,
    }

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

    let cwd = env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let git_ctx = collect_git_context(&cwd).unwrap_or_default();

    let mut repo_ctx = build_repo_context(args, &git_ctx);
    let mut exec_ctx = build_exec_context(args, &cwd);

    let mut extra = None;
    let mut idempotency_key = args.idempotency_key.clone();

    // Attempt to load context file if SNAG_CONTEXT_FILE is provided
    if let Ok(ctx_file) = env::var("SNAG_CONTEXT_FILE") {
        let content = std::fs::read_to_string(&ctx_file).map_err(|e| {
            SnagError::ContextFileInvalid(format!("Could not read context file: {}", e))
        })?;
        let parsed: ContextFile = serde_json::from_str(&content).map_err(|e| {
            SnagError::ContextFileInvalid(format!("Invalid context file JSON: {}", e))
        })?;

        if let Some(src) = parsed.source {
            // Context file overrides environment
            source = src;
        }
        if let Some(repo) = parsed.repository {
            // Override fields if present in context file
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
        if let Some(exec) = parsed.execution {
            if let Some(wid) = exec.workspace_id {
                exec_ctx.workspace_id = Some(wid);
            }
            if let Some(pid) = exec.program_id {
                exec_ctx.program_id = Some(pid);
            }
            if let Some(sid) = exec.session_id {
                exec_ctx.session_id = Some(sid);
            }
            if let Some(pid) = exec.pearl_id {
                exec_ctx.pearl_id = Some(pid);
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
        if let Some(ext) = parsed.extra {
            extra = Some(ext);
        }
        if let Some(ik) = parsed.idempotency_key
            && idempotency_key.is_none()
        {
            idempotency_key = Some(ik);
        }
    }

    // Explicit CLI arguments override context file
    if let Some(ref rid) = args.repo_id {
        repo_ctx.repository_id = Some(rid.clone());
    }
    if let Some(ref sid) = args.session_id {
        exec_ctx.session_id = Some(sid.clone());
    }
    if let Some(ref pid) = args.pearl_id {
        exec_ctx.pearl_id = Some(pid.clone());
    }
    if let Some(ref aid) = args.attempt_id {
        exec_ctx.attempt_id = Some(aid.clone());
    }

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
        session_id: None,
        pearl_id: None,
        attempt_id: None,
        affected_repos: vec![],
    };

    let (_, ctx, _) = gather_context(&dummy_args)?;

    if args.format.as_deref() == Some("json") {
        println!("{}", serde_json::to_string_pretty(&ctx)?);
    } else {
        println!("{:#?}", ctx);
    }

    Ok(())
}
