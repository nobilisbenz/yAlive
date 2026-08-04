use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};

const IGNORE_ENTRIES: &[&str] = &[
    "/.notes/index.sqlite",
    "/.notes/index.sqlite-shm",
    "/.notes/index.sqlite-wal",
    "/.notes/ygraphy-open.json",
    "/.notes/ygraphy-open.pending",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncSummary {
    pub remote: String,
    pub branch: String,
    pub committed: bool,
}

pub fn remote(vault: &Path) -> Option<String> {
    git(vault, &["remote", "get-url", "origin"])
        .ok()
        .map(|output| output.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn configure_remote(vault: &Path, repository: &str) -> Result<()> {
    validate_remote(repository)?;
    ensure_repository(vault)?;
    if remote(vault).is_some() {
        run_git(vault, &["remote", "set-url", "origin", repository])?;
    } else {
        run_git(vault, &["remote", "add", "origin", repository])?;
    }
    Ok(())
}

pub fn sync(vault: &Path, repository: Option<&str>) -> Result<SyncSummary> {
    ensure_repository(vault)?;
    ensure_gitignore(vault)?;
    if let Some(repository) = repository {
        configure_remote(vault, repository)?;
    }
    let remote = remote(vault)
        .context("no Git remote configured; provide a repository URL in Options or with --repo")?;
    validate_remote(&remote)?;

    run_git(vault, &["add", "--all"])?;
    let committed = !git_success(vault, &["diff", "--cached", "--quiet"])?;
    if committed {
        run_git(vault, &["commit", "-m", "Sync yalive vault"])
            .context("could not commit; configure Git user.name and user.email")?;
    }

    run_git(vault, &["fetch", "--prune", "origin"]).context(
        "GitHub authentication failed; use SSH or run `gh auth login` and `gh auth setup-git`",
    )?;
    let local_branch = current_branch(vault)?;
    let branch = remote_branch(vault, &local_branch).unwrap_or(local_branch);
    let remote_ref = format!("refs/remotes/origin/{branch}");
    if git_success(vault, &["show-ref", "--verify", "--quiet", &remote_ref])? {
        let upstream = format!("origin/{branch}");
        let related = git_success(vault, &["merge-base", "HEAD", &upstream])?;
        let result = if related {
            run_git(vault, &["rebase", &upstream])
        } else {
            run_git(
                vault,
                &[
                    "merge",
                    "--allow-unrelated-histories",
                    "--no-edit",
                    &upstream,
                ],
            )
        };
        if let Err(error) = result {
            let _ = if related {
                git(vault, &["rebase", "--abort"])
            } else {
                git(vault, &["merge", "--abort"])
            };
            return Err(error).context(
                "sync conflict; local files were restored and no remote changes were overwritten",
            );
        }
    }

    let refspec = format!("HEAD:{branch}");
    run_git(vault, &["push", "--set-upstream", "origin", &refspec])?;
    Ok(SyncSummary {
        remote,
        branch,
        committed,
    })
}

fn ensure_repository(vault: &Path) -> Result<()> {
    if vault.join(".git").is_dir() {
        return Ok(());
    }
    if git(vault, &["rev-parse", "--show-toplevel"]).is_ok() {
        bail!("vault is inside another Git repository; use a standalone vault directory");
    }
    run_git(vault, &["init", "--initial-branch=main"])
}

fn ensure_gitignore(vault: &Path) -> Result<()> {
    let path = vault.join(".gitignore");
    let mut source = if path.exists() {
        fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?
    } else {
        String::new()
    };
    let mut changed = false;
    for entry in IGNORE_ENTRIES {
        if !source.lines().any(|line| line.trim() == *entry) {
            if !source.is_empty() && !source.ends_with('\n') {
                source.push('\n');
            }
            source.push_str(entry);
            source.push('\n');
            changed = true;
        }
    }
    if changed {
        fs::write(&path, source).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

fn validate_remote(repository: &str) -> Result<()> {
    let repository = repository.trim();
    if repository.is_empty() {
        bail!("repository URL cannot be empty");
    }
    if let Some(rest) = repository
        .strip_prefix("https://")
        .or_else(|| repository.strip_prefix("http://"))
        && rest
            .split('/')
            .next()
            .is_some_and(|authority| authority.contains('@'))
    {
        bail!("repository URL must not contain a token; use `gh auth login` or SSH");
    }
    Ok(())
}

fn current_branch(vault: &Path) -> Result<String> {
    let branch = git(vault, &["branch", "--show-current"])?;
    let branch = branch.trim();
    if branch.is_empty() {
        bail!("Git repository has no current branch");
    }
    Ok(branch.to_string())
}

fn remote_branch(vault: &Path, local_branch: &str) -> Option<String> {
    let local_ref = format!("refs/remotes/origin/{local_branch}");
    if git_success(vault, &["show-ref", "--verify", "--quiet", &local_ref]).ok()? {
        return Some(local_branch.to_string());
    }
    let output = git(vault, &["ls-remote", "--symref", "origin", "HEAD"]).ok()?;
    output.lines().find_map(|line| {
        line.strip_prefix("ref: refs/heads/")
            .and_then(|value| value.split_whitespace().next())
            .map(str::to_string)
    })
}

fn run_git(vault: &Path, args: &[&str]) -> Result<()> {
    let output = git_output(vault, args)?;
    if output.status.success() {
        return Ok(());
    }
    let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
    bail!(
        "git {} failed: {}",
        args.join(" "),
        if message.is_empty() {
            "unknown error"
        } else {
            &message
        }
    )
}

fn git(vault: &Path, args: &[&str]) -> Result<String> {
    let output = git_output(vault, args)?;
    if !output.status.success() {
        bail!("git {} failed", args.join(" "));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn git_success(vault: &Path, args: &[&str]) -> Result<bool> {
    Ok(git_output(vault, args)?.status.success())
}

fn git_output(vault: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .args(args)
        .current_dir(vault)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .with_context(|| "running git; install Git to use vault sync")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn configure_identity(path: &Path) {
        run_git(path, &["config", "user.name", "Yalive Test"]).unwrap();
        run_git(path, &["config", "user.email", "test@yalive.local"]).unwrap();
    }

    #[test]
    fn rejects_tokens_embedded_in_urls() {
        let error = validate_remote("https://secret@github.com/user/vault.git").unwrap_err();
        assert!(error.to_string().contains("must not contain a token"));
        validate_remote("https://github.com/user/vault.git").unwrap();
        validate_remote("git@github.com:user/vault.git").unwrap();
    }

    #[test]
    fn syncs_changes_between_vaults() {
        let root = tempdir().unwrap();
        let remote_path = root.path().join("remote.git");
        fs::create_dir(&remote_path).unwrap();
        run_git(&remote_path, &["init", "--bare", "--initial-branch=main"]).unwrap();

        let first = root.path().join("first");
        fs::create_dir(&first).unwrap();
        fs::write(first.join("first.md"), "# First\n").unwrap();
        ensure_repository(&first).unwrap();
        configure_identity(&first);
        sync(&first, Some(remote_path.to_str().unwrap())).unwrap();

        let second = root.path().join("second");
        fs::create_dir(&second).unwrap();
        ensure_repository(&second).unwrap();
        configure_identity(&second);
        sync(&second, Some(remote_path.to_str().unwrap())).unwrap();
        assert_eq!(
            fs::read_to_string(second.join("first.md")).unwrap(),
            "# First\n"
        );

        fs::write(second.join("second.md"), "# Second\n").unwrap();
        sync(&second, None).unwrap();
        sync(&first, None).unwrap();
        assert_eq!(
            fs::read_to_string(first.join("second.md")).unwrap(),
            "# Second\n"
        );
        assert!(first.join(".gitignore").exists());
    }
}
