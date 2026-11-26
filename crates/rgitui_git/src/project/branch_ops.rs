//! Operations offered by the sidebar's branch context menu.
//!
//! Each runs `git` (or libgit2 for plain config and ref edits) on a background
//! thread through [`GitProject::run_branch_op`], which owns the operation
//! bookkeeping: begin, refresh the snapshot, then complete or fail.
use super::messages::error_message;
use anyhow::{Context as _, Result};
use git2::Repository;
use gpui::{AsyncApp, Context, Task, WeakEntity};
use rgitui_i18n::{msg, Message};
use std::path::Path;

use crate::types::*;

use super::argsafe::{validate_branch_name, validate_remote_name};
use super::auth::run_git_network_command;
use super::refresh::gather_refresh_data;
use super::{ensure_clean_worktree, git_command, GitProject, GitProjectEvent, RefreshData};

/// What a branch operation's background work produced.
enum BranchOpOutcome {
    /// Finished. Carries git's own output, if it had any.
    Done(Option<String>),
    /// Stopped on conflicts and waits for Continue or Abort.
    Paused(String),
}

/// Labels and identity of one branch operation.
struct BranchOp {
    kind: GitOperationKind,
    running: Message,
    done: Message,
    failed: Message,
    remote: Option<String>,
}

/// Run `git` in `repo_path` and return its stdout, or an error carrying stderr.
fn run_git(repo_path: &Path, args: &[&str]) -> Result<String> {
    let output = git_command()
        .current_dir(repo_path)
        .env("GIT_EDITOR", "true")
        .args(args)
        .output()
        .with_context(|| format!("Failed to execute git {}", args.first().unwrap_or(&"")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("{}", stderr.trim());
    }
}

/// The branch HEAD points at, or `None` when HEAD is detached.
fn checked_out_branch(repo: &Repository) -> Option<String> {
    super::head_branch_name(repo).ok()
}

/// The remote and remote branch `branch` is configured to track.
fn tracking_config(repo: &Repository, branch: &str) -> Result<(String, String)> {
    let config = repo.config()?;
    let remote = config.get_string(&format!("branch.{branch}.remote")).ok();
    let merge = config.get_string(&format!("branch.{branch}.merge")).ok();
    match (remote, merge) {
        (Some(remote), Some(merge)) => {
            let merge = merge
                .strip_prefix("refs/heads/")
                .unwrap_or(&merge)
                .to_string();
            validate_remote_name(&remote)?;
            validate_branch_name(&merge)?;
            Ok((remote, merge))
        }
        _ => anyhow::bail!("Branch '{branch}' has no upstream branch configured."),
    }
}

impl GitProject {
    /// The description stored for `branch` (`branch.<name>.description`).
    pub fn branch_description(&self, branch: &str) -> String {
        self.open_repo()
            .ok()
            .and_then(|repo| repo.config().ok())
            .and_then(|config| {
                config
                    .get_string(&format!("branch.{branch}.description"))
                    .ok()
            })
            .unwrap_or_default()
    }

    /// The commits an interactive rebase of HEAD onto `onto` would replay,
    /// newest first, as `(oid, summary, author)`.
    ///
    /// Walks first parents from HEAD down to the merge base with `onto`. Fails
    /// when that base is not on HEAD's first-parent line (merge commits in
    /// between), because the plan must be a plain linear range.
    pub fn rebase_range_onto(&self, onto: &str) -> Result<Vec<(git2::Oid, String, String)>> {
        validate_branch_name(onto)?;
        let repo = self.open_repo()?;
        let head = repo.head()?.peel_to_commit()?;
        let onto_oid = repo.revparse_single(onto)?.peel_to_commit()?.id();
        let base = repo.merge_base(head.id(), onto_oid)?;
        let mut range = Vec::new();
        let mut walk = head;
        while walk.id() != base {
            range.push((
                walk.id(),
                walk.summary()
                    .ok()
                    .flatten()
                    .unwrap_or_default()
                    .to_string(),
                walk.author().name().unwrap_or_default().to_string(),
            ));
            walk = walk.parent(0).map_err(|_| {
                anyhow::anyhow!(
                    "'{onto}' shares no linear history with HEAD, so it cannot be rebased interactively."
                )
            })?;
        }
        Ok(range)
    }

    fn run_branch_op<F>(
        &mut self,
        op: BranchOp,
        cx: &mut Context<Self>,
        work: F,
    ) -> Task<Result<()>>
    where
        F: FnOnce(&Path) -> Result<BranchOpOutcome> + Send + 'static,
    {
        let repo_path = self.repo_path.clone();
        let commit_limit = self.commit_limit;
        let head_branch = self.head_branch.clone();
        let BranchOp {
            kind,
            running,
            done,
            failed,
            remote,
        } = op;
        let operation_id = self.begin_operation(kind, running, remote.clone(), head_branch, cx);
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result: anyhow::Result<(BranchOpOutcome, RefreshData)> = cx
                .background_executor()
                .spawn(async move {
                    let outcome = work(&repo_path)?;
                    let data = gather_refresh_data(&repo_path, commit_limit)?;
                    Ok((outcome, data))
                })
                .await;
            cx.update(|cx| {
                this.update(cx, |this, cx| {
                    match result {
                        Ok((outcome, data)) => {
                            this.apply_refresh_data(data);
                            match outcome {
                                BranchOpOutcome::Done(details) => {
                                    this.complete_op(
                                        operation_id,
                                        kind,
                                        done,
                                        (
                                            details.map(Message::verbatim),
                                            remote.clone(),
                                            this.head_branch.clone(),
                                        ),
                                        cx,
                                    );
                                }
                                BranchOpOutcome::Paused(detail) => {
                                    this.fail_op(
                                        operation_id,
                                        kind,
                                        msg!("git.rebase.paused"),
                                        msg!("git.rebase.paused_detail", detail = &detail),
                                        (None, this.head_branch.clone(), false),
                                        cx,
                                    );
                                }
                            }
                            cx.emit(GitProjectEvent::RepositoryChanged);
                        }
                        Err(e) => {
                            log::error!("Branch operation failed: {}", e);
                            this.fail_op(
                                operation_id,
                                kind,
                                failed,
                                error_message(&e),
                                (remote.clone(), this.head_branch.clone(), false),
                                cx,
                            );
                        }
                    }
                    cx.notify();
                    Ok(())
                })
            })?
        })
    }

    /// Move `branch` forward to `upstream` (e.g. `origin/main`) when that is a
    /// fast-forward. The checked-out branch is merged with `--ff-only`; any
    /// other branch has its ref advanced with a local fetch, which git refuses
    /// unless it is a fast-forward.
    pub fn fast_forward_branch(
        &mut self,
        branch: &str,
        upstream: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let branch = branch.to_string();
        let upstream = upstream.to_string();
        let op = BranchOp {
            kind: GitOperationKind::Merge,
            running: msg!(
                "git.fast_forward.running",
                branch = &branch,
                upstream = &upstream
            ),
            done: msg!(
                "git.fast_forward.done",
                branch = &branch,
                upstream = &upstream
            ),
            failed: msg!("git.fast_forward.failed", branch = &branch),
            remote: None,
        };
        self.run_branch_op(op, cx, move |repo_path| {
            validate_branch_name(&branch)?;
            validate_branch_name(&upstream)?;
            let repo = Repository::open(repo_path)?;
            let upstream_ref = format!("refs/remotes/{upstream}");
            if checked_out_branch(&repo).as_deref() == Some(branch.as_str()) {
                let output = run_git(repo_path, &["merge", "--ff-only", &upstream_ref])?;
                Ok(BranchOpOutcome::Done(
                    Some(output).filter(|s| !s.is_empty()),
                ))
            } else {
                let refspec = format!("{upstream_ref}:refs/heads/{branch}");
                run_git(repo_path, &["fetch", ".", &refspec])?;
                Ok(BranchOpOutcome::Done(None))
            }
        })
    }

    /// Fetch the remote branch `branch` tracks and fast-forward `branch` to it.
    pub fn fetch_into_branch(
        &mut self,
        branch: &str,
        upstream: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let branch = branch.to_string();
        let upstream = upstream.to_string();
        let remote_name = upstream.split('/').next().map(str::to_string);
        let op = BranchOp {
            kind: GitOperationKind::Fetch,
            running: msg!(
                "git.fetch_into.running",
                branch = &branch,
                upstream = &upstream
            ),
            done: msg!(
                "git.fetch_into.done",
                branch = &branch,
                upstream = &upstream
            ),
            failed: msg!(
                "git.fetch_into.failed",
                branch = &branch,
                upstream = &upstream
            ),
            remote: remote_name,
        };
        self.run_branch_op(op, cx, move |repo_path| {
            validate_branch_name(&branch)?;
            let repo = Repository::open(repo_path)?;
            let (remote, merge) = tracking_config(&repo, &branch)?;
            let is_head = checked_out_branch(&repo).as_deref() == Some(branch.as_str());
            drop(repo);
            let merge_ref = format!("refs/heads/{merge}");
            if is_head {
                run_git_network_command(repo_path, &["fetch", &remote, &merge_ref])?;
                let output = run_git(repo_path, &["merge", "--ff-only", "FETCH_HEAD"])?;
                Ok(BranchOpOutcome::Done(
                    Some(output).filter(|s| !s.is_empty()),
                ))
            } else {
                let refspec = format!("{merge_ref}:refs/heads/{branch}");
                let details = run_git_network_command(repo_path, &["fetch", &remote, &refspec])?;
                Ok(BranchOpOutcome::Done(details))
            }
        })
    }

    /// Push a local branch that is not checked out to the branch it tracks.
    pub fn push_branch(&mut self, branch: &str, cx: &mut Context<Self>) -> Task<Result<()>> {
        let branch = branch.to_string();
        let target = self
            .open_repo()
            .ok()
            .and_then(|repo| tracking_config(&repo, &branch).ok());
        let (remote, remote_branch) = target.unwrap_or_else(|| (String::new(), branch.clone()));
        let op = BranchOp {
            kind: GitOperationKind::Push,
            running: msg!(
                "git.push.running",
                remote = &remote,
                branch = &remote_branch
            ),
            done: msg!(
                "git.push.done",
                branch = &branch,
                remote = &remote,
                remote_branch = &remote_branch
            ),
            failed: msg!("git.push.failed", remote = &remote),
            remote: Some(remote),
        };
        self.run_branch_op(op, cx, move |repo_path| {
            validate_branch_name(&branch)?;
            let repo = Repository::open(repo_path)?;
            let (remote, remote_branch) = tracking_config(&repo, &branch)?;
            drop(repo);
            let refspec = format!("refs/heads/{branch}:refs/heads/{remote_branch}");
            let details = run_git_network_command(repo_path, &["push", &remote, &refspec])?;
            Ok(BranchOpOutcome::Done(details))
        })
    }

    /// Rebase the checked-out branch onto `onto`.
    pub fn rebase_onto(&mut self, onto: &str, cx: &mut Context<Self>) -> Task<Result<()>> {
        let onto = onto.to_string();
        let op = BranchOp {
            kind: GitOperationKind::Rebase,
            running: msg!("git.rebase_onto.running", onto = &onto),
            done: msg!("git.rebase_onto.done", onto = &onto),
            failed: msg!("git.rebase_onto.failed", onto = &onto),
            remote: None,
        };
        self.run_branch_op(op, cx, move |repo_path| {
            validate_branch_name(&onto)?;
            {
                let repo = Repository::open(repo_path)?;
                ensure_clean_worktree(&repo, GitOperationKind::Rebase.name())?;
            }
            let output = git_command()
                .current_dir(repo_path)
                .env("GIT_EDITOR", "true")
                .args(["rebase", &onto])
                .output()
                .context("Failed to execute git rebase")?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            if output.status.success() {
                Ok(BranchOpOutcome::Done(
                    Some(stdout.trim().to_string()).filter(|s| !s.is_empty()),
                ))
            } else if stderr.contains("CONFLICT") || stderr.contains("could not apply") {
                let detail: String = stderr.lines().take(3).collect::<Vec<_>>().join(" ");
                Ok(BranchOpOutcome::Paused(detail))
            } else {
                anyhow::bail!("{}", stderr.trim());
            }
        })
    }

    /// Make `branch` track `upstream` (e.g. `origin/main`), or stop it
    /// tracking anything when `upstream` is `None`.
    pub fn set_branch_upstream(
        &mut self,
        branch: &str,
        upstream: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let branch = branch.to_string();
        let upstream = upstream.map(str::to_string);
        let shown = upstream.clone().unwrap_or_default();
        let op = BranchOp {
            kind: GitOperationKind::Branch,
            running: msg!("git.tracking.running", branch = &branch),
            done: msg!("git.tracking.done", branch = &branch, upstream = &shown),
            failed: msg!("git.tracking.failed", branch = &branch),
            remote: None,
        };
        self.run_branch_op(op, cx, move |repo_path| {
            validate_branch_name(&branch)?;
            if let Some(upstream) = &upstream {
                validate_branch_name(upstream)?;
            }
            let repo = Repository::open(repo_path)?;
            let mut local = repo.find_branch(&branch, git2::BranchType::Local)?;
            local.set_upstream(upstream.as_deref())?;
            Ok(BranchOpOutcome::Done(None))
        })
    }

    /// Store `description` as `branch.<name>.description`. An empty
    /// description removes the setting.
    pub fn set_branch_description(
        &mut self,
        branch: &str,
        description: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let branch = branch.to_string();
        let description = description.trim().to_string();
        let op = BranchOp {
            kind: GitOperationKind::Branch,
            running: msg!("git.description.running", branch = &branch),
            done: msg!("git.description.done", branch = &branch),
            failed: msg!("git.description.failed", branch = &branch),
            remote: None,
        };
        self.run_branch_op(op, cx, move |repo_path| {
            validate_branch_name(&branch)?;
            let repo = Repository::open(repo_path)?;
            let mut config = repo.config()?;
            let key = format!("branch.{branch}.description");
            if description.is_empty() {
                match config.remove(&key) {
                    Ok(()) => {}
                    Err(e) if e.code() == git2::ErrorCode::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            } else {
                config.set_str(&key, &description)?;
            }
            Ok(BranchOpOutcome::Done(None))
        })
    }

    /// Write the tree of `branch` to an archive at `output`. The format
    /// follows the file extension (`.zip`, `.tar`, `.tar.gz`, `.tgz`).
    pub fn archive_branch(
        &mut self,
        branch: &str,
        output: &Path,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let branch = branch.to_string();
        let output = output.to_path_buf();
        let shown = output.display().to_string();
        let op = BranchOp {
            kind: GitOperationKind::Branch,
            running: msg!("git.archive.running", branch = &branch),
            done: msg!("git.archive.done", branch = &branch, path = &shown),
            failed: msg!("git.archive.failed", branch = &branch),
            remote: None,
        };
        self.run_branch_op(op, cx, move |repo_path| {
            validate_branch_name(&branch)?;
            let output = output.to_string_lossy().to_string();
            run_git(
                repo_path,
                &["archive", "-o", &output, &format!("refs/heads/{branch}")],
            )?;
            Ok(BranchOpOutcome::Done(None))
        })
    }
}
