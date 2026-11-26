//! Workspace side of the sidebar's branch context menu: the actions that need
//! a dialog, a file picker or a diff view rather than a single git call.
use gpui::{AsyncApp, Context, WeakEntity};
use rgitui_diff::DiffSource;
use rgitui_git::{CommitInfo, Signature};

use crate::interactive_rebase::{RebaseAction, RebaseEntry};
use crate::{BranchPromptKind, LocalChangesAction, LocalChangesChoice, ToastKind};

use super::Workspace;

/// Archive file name suggested for `branch`: path separators would otherwise
/// make `feat/x` look like a directory.
fn archive_file_name(branch: &str) -> String {
    format!("{}.zip", branch.replace(['/', '\\'], "-"))
}

/// Whether the repository is stopped in the middle of a rebase.
fn is_mid_rebase(state: rgitui_git::RepoState) -> bool {
    matches!(
        state,
        rgitui_git::RepoState::Rebase
            | rgitui_git::RepoState::RebaseInteractive
            | rgitui_git::RepoState::RebaseMerge
    )
}

impl Workspace {
    /// Deal with uncommitted changes as `choice` says, run `action`, and give
    /// stashed changes back afterwards.
    ///
    /// A rebase that stops on conflicts leaves the stash in place: popping it
    /// into a half-finished rebase would only pile conflicts on conflicts.
    pub(super) fn run_with_local_changes(
        &mut self,
        action: LocalChangesAction,
        choice: LocalChangesChoice,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        let project = tab.project.clone();
        let worktree_path = self.effective_worktree_path(cx);
        cx.spawn(
            async move |this: WeakEntity<Workspace>, cx: &mut AsyncApp| {
                let mut stashed = false;
                match choice {
                    LocalChangesChoice::DoNothing => {}
                    LocalChangesChoice::StashAndReapply => {
                        let before = cx.update(|cx| project.read(cx).stashes().len());
                        let task = cx.update(|cx| {
                            project
                                .update(cx, |proj, cx| proj.stash_save_at(None, &worktree_path, cx))
                        });
                        let _ = task.await;
                        // Nothing to stash leaves the list as it was.
                        stashed = cx.update(|cx| project.read(cx).stashes().len()) > before;
                    }
                    LocalChangesChoice::Discard => {
                        let task = cx.update(|cx| {
                            project.update(cx, |proj, cx| proj.reset_hard_at(&worktree_path, cx))
                        });
                        let _ = task.await;
                    }
                }
                let task = cx.update(|cx| {
                    project.update(cx, |proj, cx| match &action {
                        LocalChangesAction::Checkout(name) => proj.checkout_branch(name, cx),
                        LocalChangesAction::Rebase(name) => proj.rebase_onto(name, cx),
                    })
                });
                let _ = task.await;
                if !stashed {
                    return;
                }
                let paused = cx.update(|cx| is_mid_rebase(project.read(cx).repo_state()));
                if paused {
                    cx.update(|cx| {
                        let _ = this.update(cx, |this, cx| {
                            this.show_toast(
                                rgitui_i18n::t!("local_changes.kept_stash").to_string(),
                                ToastKind::Warning,
                                cx,
                            );
                        });
                    });
                } else {
                    cx.update(|cx| {
                        project.update(cx, |proj, cx| {
                            proj.stash_pop_at(0, &worktree_path, cx).detach();
                        });
                    });
                }
            },
        )
        .detach();
    }

    /// Open the diff of `other` → `branch` in the diff viewer and detail panel.
    pub(super) fn compare_branch_with(
        &mut self,
        branch: String,
        other: String,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        let repo_path = tab.project.read(cx).repo_path().to_path_buf();
        let diff_viewer = tab.diff_viewer.clone();
        let detail_panel = tab.detail_panel.clone();
        cx.spawn(
            async move |this: WeakEntity<Workspace>, cx: &mut AsyncApp| {
                let (from, to) = (other.clone(), branch.clone());
                let result = cx
                    .background_executor()
                    .spawn(async move { rgitui_git::compute_range_diff(&repo_path, &from, &to) })
                    .await;
                cx.update(|cx| match result {
                    Ok((diff, from_oid, to_oid)) => {
                        let Some(first_file) = diff.files.first().cloned() else {
                            let _ = this.update(cx, |this, cx| {
                                this.show_toast(
                                    rgitui_i18n::t!(
                                        "branch_compare.no_differences",
                                        branch = branch.as_str(),
                                        other = other.as_str()
                                    )
                                    .to_string(),
                                    ToastKind::Info,
                                    cx,
                                );
                            });
                            return;
                        };
                        let source = DiffSource::Compare {
                            from: from_oid.to_string(),
                            to: to_oid.to_string(),
                        };
                        let path = first_file.path.display().to_string();
                        diff_viewer.update(cx, |dv, cx| dv.set_diff(first_file, path, source, cx));
                        let title = format!("{other} \u{2192} {branch}");
                        let who = Signature {
                            name: String::from("compare"),
                            email: String::new(),
                        };
                        let info = CommitInfo {
                            oid: to_oid,
                            short_id: to_oid.to_string()[..7].to_string(),
                            summary: title.clone(),
                            message: title,
                            author: who.clone(),
                            committer: who,
                            co_authors: vec![],
                            time: chrono::Utc::now(),
                            parent_oids: vec![from_oid],
                            refs: vec![],
                            is_signed: false,
                        };
                        detail_panel.update(cx, |dp, cx| {
                            dp.set_comparison(
                                info,
                                diff,
                                from_oid.to_string(),
                                to_oid.to_string(),
                                cx,
                            )
                        });
                    }
                    Err(e) => {
                        let _ = this.update(cx, |this, cx| {
                            this.show_toast(
                                rgitui_i18n::t!("branch_compare.failed", error = e.to_string())
                                    .to_string(),
                                ToastKind::Error,
                                cx,
                            );
                        });
                    }
                });
            },
        )
        .detach();
    }

    /// Open the interactive rebase editor for the checked-out branch onto
    /// `onto`.
    pub(super) fn start_interactive_rebase_onto(&mut self, onto: String, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        let range = tab.project.read(cx).rebase_range_onto(&onto);
        match range {
            Ok(range) if range.is_empty() => {
                self.show_toast(
                    rgitui_i18n::t!("branch_rebase.nothing_to_rebase", onto = onto.as_str())
                        .to_string(),
                    ToastKind::Info,
                    cx,
                );
            }
            Ok(range) => {
                let entries: Vec<RebaseEntry> = range
                    .into_iter()
                    .map(|(oid, summary, author)| RebaseEntry {
                        oid: oid.to_string(),
                        original_message: summary,
                        author,
                        action: RebaseAction::Pick,
                    })
                    .collect();
                self.overlays.interactive_rebase.update(cx, |ir, cx| {
                    ir.show_visible(entries, format!("{onto} (rebase onto)"), cx);
                    ir.set_onto(Some(onto));
                });
            }
            Err(e) => self.show_toast(e.to_string(), ToastKind::Warning, cx),
        }
    }

    /// Ask where to save an archive of `branch`, then write it.
    pub(super) fn archive_branch_prompt(&mut self, branch: String, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        let project = tab.project.clone();
        let repo_path = project.read(cx).repo_path().to_path_buf();
        let directory = repo_path.parent().unwrap_or(&repo_path).to_path_buf();
        let picked = cx.prompt_for_new_path(&directory, Some(&archive_file_name(&branch)));
        cx.spawn(
            async move |_this: WeakEntity<Workspace>, cx: &mut AsyncApp| {
                if let Ok(Ok(Some(path))) = picked.await {
                    cx.update(|cx| {
                        project.update(cx, |proj, cx| {
                            proj.archive_branch(&branch, &path, cx).detach();
                        });
                    });
                }
            },
        )
        .detach();
    }

    /// Ask the user to pick the remote branch `branch` should track.
    pub(super) fn set_tracking_prompt(&mut self, branch: String, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        let candidates: Vec<String> = tab
            .project
            .read(cx)
            .branches()
            .iter()
            .filter(|b| b.is_remote && !b.name.ends_with("/HEAD"))
            .map(|b| b.name.clone())
            .collect();
        if candidates.is_empty() {
            self.show_toast(
                rgitui_i18n::t!("branch_prompt.no_remote_branches").to_string(),
                ToastKind::Warning,
                cx,
            );
            return;
        }
        self.dialogs.branch_prompt_dialog.update(cx, |d, cx| {
            d.show_picker(BranchPromptKind::TrackingBranch, branch, candidates, cx);
        });
    }

    /// Ask the user which ref to compare `branch` with.
    pub(super) fn compare_with_prompt(&mut self, branch: String, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        let project = tab.project.read(cx);
        let mut candidates: Vec<String> = project
            .branches()
            .iter()
            .filter(|b| b.name != branch && !b.name.ends_with("/HEAD"))
            .map(|b| b.name.clone())
            .collect();
        candidates.extend(project.tags().iter().map(|t| t.name.clone()));
        self.dialogs.branch_prompt_dialog.update(cx, |d, cx| {
            d.show_picker(BranchPromptKind::CompareWith, branch, candidates, cx);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::archive_file_name;

    #[test]
    fn archive_name_flattens_separators() {
        assert_eq!(archive_file_name("feat/DSIP-1"), "feat-DSIP-1.zip");
    }

    #[test]
    fn archive_name_keeps_plain_names() {
        assert_eq!(archive_file_name("main"), "main.zip");
    }
}
