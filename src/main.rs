use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use directories::{ProjectDirs, UserDirs};
use serde_json::json;

use yalive::app;
use yalive::db::Database;
use yalive::model::{card_capabilities, relation_capabilities};
use yalive::sync;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Markdown vault directory.
    #[arg(short, long, global = true)]
    vault: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Rebuild changed parts of the disposable index.
    Index,
    /// Print parser and link/card diagnostics.
    Diagnostics,
    /// Export portable review history as JSON Lines.
    ExportReviews {
        /// Output path; defaults to .notes/reviews.jsonl.
        output: Option<PathBuf>,
    },
    /// Safely pull, merge, commit, and push the vault with Git.
    Sync {
        /// GitHub repository URL. Needed only for initial setup or to change the remote.
        #[arg(long)]
        repo: Option<String>,
    },
    /// Machine-readable commands for editor integrations.
    Editor {
        #[command(subcommand)]
        command: EditorCommand,
    },
}

#[derive(Subcommand)]
enum EditorCommand {
    /// Describe supported card and relation syntax.
    Capabilities,
    /// Search all indexed notes and sections.
    Sections {
        /// Full-text query; omit it to list every section.
        #[arg(default_value = "")]
        query: String,
    },
    /// List incoming and outgoing relations for a section.
    Relations { section_uid: String },
    /// Return diagnostics as JSON.
    Diagnostics,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let vault = resolve_vault(cli.vault)?;
    remember_vault(&vault)?;
    match cli.command {
        None => run_app(vault),
        Some(Command::Index) => {
            let mut db = Database::open(&vault)?;
            let summary = db.index_vault(&vault)?;
            println!(
                "indexed={} unchanged={} removed={} failed={} diagnostics={}",
                summary.indexed,
                summary.unchanged,
                summary.removed,
                summary.failed,
                summary.diagnostics
            );
            Ok(())
        }
        Some(Command::Diagnostics) => {
            let mut db = Database::open(&vault)?;
            db.index_vault(&vault)?;
            for diagnostic in db.diagnostics()? {
                println!(
                    "{}:{}: {}",
                    diagnostic.path.display(),
                    diagnostic.line,
                    diagnostic.message
                );
            }
            Ok(())
        }
        Some(Command::ExportReviews { output }) => {
            let db = Database::open(&vault)?;
            let output = output.unwrap_or_else(|| vault.join(".notes/reviews.jsonl"));
            let count = db.export_reviews(&output)?;
            println!("exported {count} reviews to {}", output.display());
            Ok(())
        }
        Some(Command::Sync { repo }) => {
            let summary = sync::sync(&vault, repo.as_deref())?;
            println!(
                "synced {} on {}{}",
                summary.remote,
                summary.branch,
                if summary.committed {
                    " (committed local changes)"
                } else {
                    ""
                }
            );
            Ok(())
        }
        Some(Command::Editor { command }) => run_editor_command(&vault, command),
    }
}

fn run_editor_command(vault: &Path, command: EditorCommand) -> Result<()> {
    if let EditorCommand::Capabilities = &command {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "protocol_version": 1,
                "card_types": card_capabilities(),
                "relation_types": relation_capabilities(),
            }))?
        );
        return Ok(());
    }

    let mut db = Database::open(vault)?;
    db.index_vault(vault)?;
    let value = match command {
        EditorCommand::Capabilities => unreachable!(),
        EditorCommand::Sections { query } => json!({
            "protocol_version": 1,
            "items": db.search(&query)?,
        }),
        EditorCommand::Relations { section_uid } => json!({
            "protocol_version": 1,
            "items": db.relations(&section_uid)?,
        }),
        EditorCommand::Diagnostics => json!({
            "protocol_version": 1,
            "items": db.diagnostics()?,
        }),
    };
    println!("{}", serde_json::to_string(&value)?);
    Ok(())
}

fn run_app(mut vault: PathBuf) -> Result<()> {
    loop {
        remember_vault(&vault)?;
        match app::run(&vault)? {
            Some(next) => vault = next,
            None => return Ok(()),
        }
    }
}

fn resolve_vault(requested: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = requested {
        return canonical_vault(&path);
    }
    if let Some(path) = remembered_vault()?
        && path.is_dir()
    {
        return canonical_vault(&path);
    }
    prompt_for_vault()
}

fn prompt_for_vault() -> Result<PathBuf> {
    println!("No saved yalive vault was found.");
    loop {
        let action = prompt("[o] Open existing vault  [c] Create new vault: ")?;
        let create = match action.trim().to_ascii_lowercase().as_str() {
            "o" | "open" => false,
            "c" | "create" => true,
            _ => {
                println!("Enter o to open or c to create.");
                continue;
            }
        };
        let value = prompt("Vault path: ")?;
        let path = expand_home(value.trim())?;
        if create {
            fs::create_dir_all(&path)
                .with_context(|| format!("creating vault {}", path.display()))?;
        }
        if !path.is_dir() {
            println!("Directory does not exist: {}", path.display());
            continue;
        }
        let path = canonical_vault(&path)?;
        remember_vault(&path)?;
        return Ok(path);
    }
}

fn prompt(message: &str) -> Result<String> {
    print!("{message}");
    io::stdout().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    Ok(value)
}

fn canonical_vault(path: &Path) -> Result<PathBuf> {
    let path = path
        .canonicalize()
        .with_context(|| format!("opening vault {}", path.display()))?;
    if !path.is_dir() {
        anyhow::bail!("vault is not a directory: {}", path.display());
    }
    Ok(path)
}

fn expand_home(value: &str) -> Result<PathBuf> {
    let path = if value == "~" || value.starts_with("~/") {
        let home = UserDirs::new()
            .map(|directories| directories.home_dir().to_path_buf())
            .context("could not determine home directory")?;
        if value == "~" {
            home
        } else {
            home.join(&value[2..])
        }
    } else {
        PathBuf::from(value)
    };
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn remembered_vault() -> Result<Option<PathBuf>> {
    let path = state_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let value = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let value = value.trim();
    Ok((!value.is_empty()).then(|| PathBuf::from(value)))
}

fn remember_vault(vault: &Path) -> Result<()> {
    let path = state_path()?;
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(&path, vault.to_string_lossy().as_bytes())
        .with_context(|| format!("writing {}", path.display()))
}

fn state_path() -> Result<PathBuf> {
    let directories = ProjectDirs::from("dev", "yalive", "yalive")
        .context("could not determine yalive configuration directory")?;
    Ok(directories.config_dir().join("last-vault"))
}
