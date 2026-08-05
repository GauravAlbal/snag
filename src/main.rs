pub mod artifacts;
pub mod backup;
pub mod cli;
pub mod context;
pub mod doctor;
pub mod error;
pub mod export;
pub mod git;
pub mod idempotency;
pub mod identity;
pub mod migrations;
pub mod parser;
pub mod rebuild;
pub mod record;
pub mod report;
pub mod restore;
pub mod schema;
pub mod store;
pub mod types;
pub mod verify;
use clap::Parser;
use cli::{Cli, Command};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    let wants_json = cli.wants_json();

    let result = match cli.command {
        Some(Command::Report(args)) => report::handle(args),
        Some(Command::List(args)) => report::list(args),
        Some(Command::Show(args)) => report::show(args),
        Some(Command::Context(args)) => context::handle(args),
        Some(Command::Export(args)) => export::handle(args),
        Some(Command::Backup(args)) => backup::handle(args),
        Some(Command::Verify(args)) => verify::handle(args),
        Some(Command::Doctor(args)) => doctor::handle(args),
        Some(Command::Retract(args)) => report::retract(args),
        Some(Command::Restore(args)) => restore::handle(args),
        Some(Command::Rebuild(args)) => rebuild::handle(args),
        None => {
            // "snag <title>" fast path, equivalent to "snag report <title>"
            if let Some(title) = cli.title {
                report::handle(cli::ReportArgs {
                    title: Some(title),
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
                })
            } else {
                // Should be unreachable due to clap validation, but just in case
                Err(anyhow::anyhow!("Error: A title or command is required."))
            }
        }
    };

    if let Err(e) = result {
        if wants_json {
            let snag_error = match e.downcast::<error::SnagError>() {
                Ok(se) => se.to_envelope(),
                Err(e) => error::SnagError::Other(e).to_envelope(),
            };
            eprintln!("{}", serde_json::to_string_pretty(&snag_error).unwrap());
            std::process::exit(1);
        } else {
            return Err(e);
        }
    }

    Ok(())
}
