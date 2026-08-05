use crate::cli::InitArgs;
use anyhow::{Context, Result};
use std::path::PathBuf;

/// Canonical capture-and-move-on instruction block installed by `snag init`.
/// Kept in one place so the binary, the examples, and the docs never drift.
pub const INSTRUCTION_BLOCK: &str = r#"When you encounter a material bug, regression, misleading interface, recurring
workaround, or tooling failure outside the current task, record it with `snag`
while the evidence is fresh, then continue the assigned task.

Do not broaden the current task to fix the snag unless explicitly asked.
Do not report ordinary implementation errors or your own transient mistakes.
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

pub fn handle(args: InitArgs) -> Result<()> {
    let path = args
        .file
        .clone()
        .unwrap_or_else(|| PathBuf::from("AGENTS.md"));
    let agent = args.agent.as_deref().unwrap_or("generic");

    let existing = std::fs::read_to_string(&path).unwrap_or_default();
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
    std::fs::write(&path, content).with_context(|| format!("cannot write {}", path.display()))?;
    println!(
        "Configured {} with the snag capture-and-move-on instruction block.",
        path.display()
    );
    println!("Run `snag doctor` to confirm the store, then report with `snag report \"<title>\"`.");
    Ok(())
}
