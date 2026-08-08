use crate::error::SnagError;
use crate::git::{GitContext, normalize_remote_alias};
use crate::store::Store;
use crate::types::generate_id;
use rusqlite::{OptionalExtension, params};

/// Role of a repository in an observation: the repository the observation was
/// made from (primary) or a repository it is understood to affect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoRole {
    /// The filing context — the repo the reporter was in when the observation
    /// was captured.
    Reporter,
    /// The lane that owns the fix (explicit `--owner`).
    Owner,
    /// Additional repos implicated by the observation.
    Affected,
}

impl RepoRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            RepoRole::Reporter => "reporter",
            RepoRole::Owner => "owner",
            RepoRole::Affected => "affected",
        }
    }
}

/// Structured result of resolving a repository identity.
#[derive(Debug)]
pub struct RepositoryResolution {
    pub repository_id: String,
    pub checkout_id: Option<String>,
    pub worktree_id: Option<String>,
    pub warnings: Vec<String>,
}

/// Resolve an explicit repository ID. An explicit ID is honored: if the repo
/// does not exist it is created (documented rule: the caller explicitly names
/// the repository, so the identity is created and linked).
pub(crate) fn ensure_explicit_repo(
    store: &mut Store,
    id: &str,
    now: &str,
) -> anyhow::Result<String> {
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let exists: i64 = tx.query_row(
        "SELECT COUNT(*) FROM repositories WHERE repository_id = ?1",
        params![id],
        |r| r.get(0),
    )?;
    if exists == 0 {
        tx.execute(
            "INSERT INTO repositories (repository_id, created_at) VALUES (?1, ?2)",
            params![id, now],
        )?;
    }
    tx.commit()?;
    Ok(id.to_string())
}

/// Record that `repo_id` is a candidate for each of the given aliases (G30).
/// Multiple repositories may share an alias; only a single candidate is
/// unambiguous.
fn record_aliases(store: &mut Store, aliases: &[String], repo_id: &str, now: &str) {
    if aliases.is_empty() {
        return;
    }
    let tx = match store.conn.transaction() {
        Ok(t) => t,
        Err(_) => return,
    };
    for alias in aliases {
        let norm = normalize_remote_alias(alias);
        // Bump last_seen_at when the (alias, repo) pair already exists — the
        // PK is composite, so a same-alias-different-repo row is a distinct
        // row, not a conflict. Re-seen aliases bump last_seen_at so the
        // display heuristic (most-recently-seen) tracks the live remote: a
        // fleet rename makes the new org's alias win after the first
        // post-rename filing.
        let _ = tx.execute(
            "INSERT INTO repository_aliases (alias, repository_id, confirmed, first_seen_at, last_seen_at)
             VALUES (?1, ?2, 1, ?3, ?3)
             ON CONFLICT(alias, repository_id) DO UPDATE SET
                confirmed = 1,
                last_seen_at = excluded.last_seen_at",
            params![norm, repo_id, now],
        );
    }
    let _ = tx.commit();
}

/// Ensure a checkout row exists for the given repo + git common dir.
fn ensure_checkout_for(
    store: &mut Store,
    git_ctx: &GitContext,
    repo_id: &str,
    now: &str,
) -> Option<String> {
    let git_common_dir = git_ctx.git_common_dir.clone()?;
    let conn = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .ok()?;
    let existing: Option<String> = conn
        .query_row(
            "SELECT checkout_id FROM checkouts WHERE git_common_dir = ?1",
            params![&git_common_dir],
            |r| r.get(0),
        )
        .optional()
        .ok()?
        .flatten();
    if existing.is_some() {
        conn.commit().ok();
        return existing;
    }
    let checkout_id = generate_id("chk");
    let res = conn.execute(
        "INSERT INTO checkouts (checkout_id, repository_id, git_common_dir, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![&checkout_id, repo_id, &git_common_dir, now],
    );
    conn.commit().ok();
    match res {
        Ok(_) => Some(checkout_id),
        Err(_) => existing,
    }
}

fn ensure_worktree_for(
    store: &mut Store,
    git_ctx: &GitContext,
    checkout_id: Option<&str>,
    now: &str,
) -> Option<String> {
    let checkout_id = checkout_id?;
    let root = git_ctx.repository_root.clone()?;
    let conn = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .ok()?;
    if let Ok(Some(wt)) = conn
        .query_row(
            "SELECT worktree_id FROM worktrees WHERE worktree_path = ?1",
            params![root],
            |r| r.get(0),
        )
        .optional()
    {
        conn.commit().ok();
        return wt;
    }
    let wt_id = generate_id("wt");
    let res = conn.execute(
        "INSERT INTO worktrees (worktree_id, checkout_id, worktree_path, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![&wt_id, checkout_id, &root, now],
    );
    conn.commit().ok();
    match res {
        Ok(_) => Some(wt_id),
        Err(_) => None,
    }
}

/// Precedence step 4: an existing checkout binding for this git common dir.
fn resolve_by_checkout(store: &mut Store, dir: &str) -> anyhow::Result<Option<String>> {
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.query_row(
        "SELECT repository_id FROM checkouts WHERE git_common_dir = ?1",
        params![dir],
        |r| r.get(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Precedence step 6: mint a brand new repository identity.
fn create_new_repo(store: &mut Store, now: &str) -> anyhow::Result<String> {
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let rid = generate_id("repo");
    tx.execute(
        "INSERT INTO repositories (repository_id, created_at) VALUES (?1, ?2)",
        params![&rid, now],
    )?;
    tx.commit()?;
    Ok(rid)
}

/// Precedence steps 4-6 for a repository with a known git common dir: known
/// checkout binding, then unique remote alias, then a new identity (which
/// appends a warning).
fn resolve_from_git_dir(
    store: &mut Store,
    git_ctx: &GitContext,
    dir: &str,
    now: &str,
    warnings: &mut Vec<String>,
) -> anyhow::Result<String> {
    // 4. Known checkout binding.
    if let Some(rid) = resolve_by_checkout(store, dir)? {
        return Ok(rid);
    }
    // 5. Unique remote alias.
    if let Some(rid) = unique_alias_match(store, &git_ctx.git_remote_aliases)? {
        return Ok(rid);
    }
    // 6. New identity.
    let rid = create_new_repo(store, now)?;
    warnings
        .push("created a new repository identity (no known checkout or unique alias)".to_string());
    Ok(rid)
}

/// Primary repository resolution with G28 precedence:
///   1. explicit CLI repository ID
///   2. context-file repository ID
///   3. environment repository ID
///   4. known checkout binding (git common dir)
///   5. unique remote alias
///   6. new repository identity
///
/// The caller merges explicit/context/env precedence into `explicit_repo_id`.
pub fn resolve_repository(
    store: &mut Store,
    git_ctx: &GitContext,
    explicit_repo_id: Option<&str>,
) -> anyhow::Result<RepositoryResolution> {
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let mut warnings = git_ctx.warnings.clone();

    // 1-3. Explicit identity.
    let repository_id = if let Some(explicit) = explicit_repo_id {
        ensure_explicit_repo(store, explicit, &now)?
    } else if let Some(dir) = &git_ctx.git_common_dir {
        resolve_from_git_dir(store, git_ctx, dir, &now, &mut warnings)?
    } else {
        return Ok(RepositoryResolution {
            repository_id: String::new(),
            checkout_id: None,
            worktree_id: None,
            warnings,
        });
    };

    // Link this repo to the current checkout/worktree and record aliases.
    let checkout_id = ensure_checkout_for(store, git_ctx, &repository_id, &now);
    let worktree_id = ensure_worktree_for(store, git_ctx, checkout_id.as_deref(), &now);
    record_aliases(store, &git_ctx.git_remote_aliases, &repository_id, &now);

    Ok(RepositoryResolution {
        repository_id,
        checkout_id,
        worktree_id,
        warnings,
    })
}

/// If the given aliases resolve to exactly one repository across all aliases,
/// return it. If multiple distinct repositories match, ambiguity is surfaced
/// (G30: multiple matches must not silently choose the first).
fn unique_alias_match(store: &mut Store, aliases: &[String]) -> anyhow::Result<Option<String>> {
    let mut candidates: Vec<String> = Vec::new();
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    for alias in aliases {
        let norm = normalize_remote_alias(alias);
        let mut stmt = tx.prepare(
            "SELECT repository_id FROM repository_aliases WHERE alias = ?1 ORDER BY repository_id",
        )?;
        let mut rows = stmt.query(params![norm])?;
        while let Some(row) = rows.next()? {
            let rid: String = row.get(0)?;
            if !candidates.contains(&rid) {
                candidates.push(rid);
            }
        }
    }
    if candidates.len() > 1 {
        return Err(SnagError::RepositoryAmbiguous(format!(
            "aliases {:?} match multiple repositories: {:?}",
            aliases, candidates
        ))
        .into());
    }
    Ok(candidates.pop())
}

/// Resolve a single `--affected-repo` value. Accepted forms: a repository ID,
/// a local path (resolved through its git common dir), an unambiguous
/// normalized remote alias, or `current`.
pub fn resolve_affected_repository(
    store: &mut Store,
    value: &str,
    git_ctx: &GitContext,
) -> anyhow::Result<String> {
    if value == "current" {
        let res = resolve_repository(store, git_ctx, None)?;
        if git_ctx.git_common_dir.is_none() {
            return Err(SnagError::RepositoryNotFound(
                "current: not inside a git repository".to_string(),
            )
            .into());
        }
        return Ok(res.repository_id);
    }

    // Local path form.
    let p = std::path::Path::new(value);
    if p.exists() && p.join(".git").exists() {
        let sub = if p.is_absolute() {
            p.to_path_buf()
        } else if value == "." {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join(value)
        };
        let resolved = crate::git::collect_git_context(&sub).map_err(|_| {
            SnagError::RepositoryInvalid(format!("could not inspect path {}", value))
        })?;
        if resolved.git_common_dir.is_none() {
            return Err(SnagError::RepositoryNotFound(format!(
                "{} is not inside a git repository",
                value
            ))
            .into());
        }
        return resolve_repository(store, &resolved, None).map(|r| r.repository_id);
    }

    // Alias form.
    let norm = normalize_remote_alias(value);
    if let Some(rid) = unique_alias_match(store, &[norm])? {
        return Ok(rid);
    }

    // Repository ID form.
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let exists: i64 = tx.query_row(
        "SELECT COUNT(*) FROM repositories WHERE repository_id = ?1",
        params![value],
        |r| r.get(0),
    )?;
    if exists > 0 {
        return Ok(value.to_string());
    }
    Err(SnagError::RepositoryNotFound(format!(
        "{} is not a known repository, alias, or existing path",
        value
    ))
    .into())
}
