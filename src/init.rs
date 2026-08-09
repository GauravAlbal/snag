use crate::cli::InitArgs;
use crate::error::SnagError;
use anyhow::Result;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

/// Canonical capture-and-move-on instruction block installed by `snag init`.
/// Kept in one place so the binary, the examples, and the docs never drift.
pub const INSTRUCTION_BLOCK: &str = r#"When you find a bug, broken command, misleading success message, recurring
workaround, or tool failure outside the current task:

1. Decide who owns the fix BEFORE recording:
   - Known: pass `--owner <owner/repo>` (id, alias, path, or `current`).
   - Genuinely ambiguous / environmental: pass `--unowned`.
   Your reporting location is NOT ownership; guessing `current` recreates the
   misrouting the explicit owner flag exists to prevent.
2. Record it with `snag` while the evidence is fresh.
3. Continue the current task.

Do not fix the unrelated problem unless the user explicitly asks you to.
Do not report ordinary implementation mistakes that belong to the current task.
Empty owner and `unowned: false` do not satisfy the requirement; one of the
two flags above is always required. JSON intake (`--json`) uses schema v2 with
exactly one of `"owner": "..."` or `"unowned": true`.
"#;

const MARKER_OPEN: &str = "<!-- snag:instructions -->";
const MARKER_CLOSE: &str = "<!-- /snag:instructions -->";

/// Agents that get a tailored setup note next to the instruction block.
const KNOWN_AGENTS: [&str; 4] = ["claude-code", "codex", "gemini-cli", "opencode"];

fn agent_setup_note(agent: &str) -> Option<String> {
    if !KNOWN_AGENTS.contains(&agent) {
        return None;
    }
    Some(
        "Set SNAG_SOURCE_KIND=agent_report and SNAG_REPORTER_ID=<agent> to mark captures as \
         agent-produced, or write a per-session SNAG_CONTEXT_FILE (see docs/SCHEMAS.md)."
            .to_string(),
    )
}

fn build_section(agent: &str) -> String {
    let mut section = String::from(MARKER_OPEN);
    section.push('\n');
    section.push_str(INSTRUCTION_BLOCK);
    if let Some(note) = agent_setup_note(agent) {
        section.push_str(&note);
        section.push('\n');
    }
    section.push_str(MARKER_CLOSE);
    section.push('\n');
    section
}
fn validation_error(path: &Path, reason: impl std::fmt::Display) -> anyhow::Error {
    SnagError::Validation(format!("{}: {reason}", path.display())).into()
}

fn open_existing(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    options.open(path)
}

fn read_existing(path: &Path) -> Result<Option<String>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(validation_error(
                path,
                format!("could not inspect target: {error}"),
            ));
        }
    };

    if metadata.file_type().is_symlink() {
        return Err(validation_error(path, "refusing to follow a symlink"));
    }
    if !metadata.file_type().is_file() {
        return Err(validation_error(
            path,
            "target must be a regular file or not exist",
        ));
    }

    let mut file = match open_existing(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(validation_error(
                path,
                format!("could not open target: {error}"),
            ));
        }
    };
    let opened_metadata = file
        .metadata()
        .map_err(|error| validation_error(path, format!("could not inspect target: {error}")))?;
    if !opened_metadata.file_type().is_file() {
        return Err(validation_error(
            path,
            "target must be a regular file or not exist",
        ));
    }

    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|error| validation_error(path, format!("could not read target: {error}")))?;
    Ok(Some(content))
}

fn publish(path: &Path, content: &str) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::Builder::new()
        .prefix(".snag-init-")
        .tempfile_in(parent)
        .map_err(|error| {
            validation_error(path, format!("could not create temporary file: {error}"))
        })?;
    temporary
        .write_all(content.as_bytes())
        .map_err(|error| validation_error(path, format!("could not write target: {error}")))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| validation_error(path, format!("could not sync target: {error}")))?;
    temporary
        .persist(path)
        .map_err(|error| validation_error(path, format!("could not publish target: {error}")))?;
    Ok(())
}

pub fn handle(args: InitArgs) -> Result<()> {
    let path = args
        .file
        .clone()
        .unwrap_or_else(|| PathBuf::from("AGENTS.md"));
    let agent = args.agent.as_deref().unwrap_or("generic");

    let existing = read_existing(&path)?.unwrap_or_default();
    if existing.contains(MARKER_OPEN) {
        println!(
            "Already configured: {} contains the snag instruction block.",
            path.display()
        );
        return Ok(());
    }

    let section = build_section(agent);
    if args.dry_run {
        println!("Would write to {}:", path.display());
        print!("{section}");
        return Ok(());
    }

    let mut content = existing.trim_end().to_string();
    if !content.is_empty() {
        content.push_str("\n\n");
    }
    content.push_str(&section);
    publish(&path, &content)?;
    println!(
        "Configured {} with the snag capture-and-move-on instruction block.",
        path.display()
    );

    println!(
        "Run `snag doctor` to confirm the store, then report with \
         `snag report \"<title>\" --owner <repo>` (or `--unowned` when genuinely ambiguous)."
    );
    Ok(())
}
