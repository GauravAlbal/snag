use crate::store::Store;
use crate::git::GitContext;
use crate::types::generate_id;
use rusqlite::{params, OptionalExtension};

pub fn resolve_repository(
    store: &mut Store,
    git_ctx: &GitContext,
) -> anyhow::Result<Option<String>> {
    if git_ctx.repository_root.is_none() && git_ctx.git_common_dir.is_none() {
        return Ok(None);
    }

    let tx = store.conn.transaction()?;
    let now = time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap();

    let git_common_dir = match &git_ctx.git_common_dir {
        Some(dir) => dir,
        None => return Ok(None),
    };

    // 1. Check if checkout exists
    let existing_checkout: Option<(String, String)> = tx.query_row(
        "SELECT checkout_id, repository_id FROM checkouts WHERE git_common_dir = ?1",
        params![git_common_dir],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;

    let (checkout_id, repository_id) = match existing_checkout {
        Some(c) => c,
        None => {
            // Check if any remote aliases match an existing repository alias
            let mut found_repo = None;
            for alias in &git_ctx.git_remote_aliases {
                if let Ok(Some(repo_id)) = tx.query_row(
                    "SELECT repository_id FROM repository_aliases WHERE alias = ?1",
                    params![alias],
                    |row| row.get(0),
                ).optional() {
                    found_repo = Some(repo_id);
                    break;
                }
            }

            let repo_id = match found_repo {
                Some(id) => id,
                None => {
                    let new_repo_id = generate_id("repo");
                    tx.execute(
                        "INSERT INTO repositories (repository_id, created_at) VALUES (?1, ?2)",
                        params![&new_repo_id, &now],
                    )?;
                    new_repo_id
                }
            };

            let new_checkout_id = generate_id("chk");
            tx.execute(
                "INSERT INTO checkouts (checkout_id, repository_id, git_common_dir, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![&new_checkout_id, &repo_id, git_common_dir, &now],
            )?;

            (new_checkout_id, repo_id)
        }
    };

    // Update aliases
    for alias in &git_ctx.git_remote_aliases {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM repository_aliases WHERE alias = ?1)",
            params![alias],
            |row| row.get(0),
        )?;
        if !exists {
            tx.execute(
                "INSERT INTO repository_aliases (alias, repository_id, first_seen_at, last_seen_at) VALUES (?1, ?2, ?3, ?4)",
                params![alias, &repository_id, &now, &now],
            )?;
        } else {
            tx.execute(
                "UPDATE repository_aliases SET last_seen_at = ?1 WHERE alias = ?2",
                params![&now, alias],
            )?;
        }
    }

    // Upsert worktree
    if let Some(root) = &git_ctx.repository_root {
        let existing_wt: Option<String> = tx.query_row(
            "SELECT worktree_id FROM worktrees WHERE worktree_path = ?1",
            params![root],
            |row| row.get(0),
        ).optional()?;

        if existing_wt.is_none() {
            let wt_id = generate_id("wt");
            tx.execute(
                "INSERT INTO worktrees (worktree_id, checkout_id, worktree_path, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![&wt_id, &checkout_id, root, &now],
            )?;
        }
    }

    tx.commit()?;
    Ok(Some(repository_id))
}
