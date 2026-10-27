//! Server-owned Git Manager guard evaluation.

use std::collections::{BTreeMap, HashSet};

use super::refs::{
    GitManagerBlockedReason, GitManagerInProgressKind, GitManagerInProgressOperation,
    GitManagerRefEntry, GitManagerRefsSnapshot,
};

const MUTATING_OPERATIONS: &[&str] = &[
    "abort",
    "branch-checkout",
    "branch-create",
    "branch-delete",
    "branch-rename",
    "checkout",
    "cherry-pick",
    "commit",
    "commit-to-branch",
    "continue",
    "delete-branch",
    "discard",
    "discard-partial",
    "fetch",
    "force-move",
    "force-push",
    "merge",
    "merge-into",
    "publish-branch",
    "pull",
    "push",
    "rebase",
    "rename-branch",
    "reorder",
    "reset",
    "resolve-conflict",
    "revert",
    "squash",
    "squash-merge",
    "stage-partial",
    "stash-apply",
    "stash-drop",
    "stash-pop",
    "stash-push",
    "tag-create",
    "tag-delete",
    "tag-push",
    "undo-commit",
    "unstage-partial",
];
const RECOVERY_OPERATIONS: &[&str] = &["abort", "continue", "resolve-conflict"];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Occupancy {
    Free,
    Worktree(String),
    MissingWorktree(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuardedRef {
    pub reference: GitManagerRefEntry,
    pub occupancy: Occupancy,
    pub remote_configured: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockedCode {
    WorktreeCheckedOut,
    DirtyWorkingTree,
    OperationInFlight,
    MergeInProgress,
    NoUpstream,
    NoRemote,
    DetachedHead,
    CurrentBranch,
    DefaultBranch,
}

impl BlockedCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WorktreeCheckedOut => "worktree-checked-out",
            Self::DirtyWorkingTree => "dirty-working-tree",
            Self::OperationInFlight => "operation-in-flight",
            Self::MergeInProgress => "merge-in-progress",
            Self::NoUpstream => "no-upstream",
            Self::NoRemote => "no-remote",
            Self::DetachedHead => "detached-head",
            Self::CurrentBranch => "current-branch",
            Self::DefaultBranch => "default-branch",
        }
    }
}

pub type BlockedReason = GitManagerBlockedReason;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuardInput {
    pub refs: Vec<GuardedRef>,
    pub dirty: bool,
    pub default_branch: Option<String>,
    pub current_branch: Option<String>,
    pub in_progress: Option<GitManagerInProgressOperation>,
    pub lock_held: bool,
}

impl GuardInput {
    pub fn from_snapshot(snapshot: &GitManagerRefsSnapshot, lock_held: bool) -> Self {
        let remote_configured = !snapshot.remotes.is_empty();
        let prunable_paths = snapshot
            .worktrees
            .iter()
            .filter(|worktree| worktree.prunable)
            .map(|worktree| worktree.path.as_str())
            .collect::<HashSet<_>>();
        let refs = snapshot
            .local_branches
            .iter()
            .cloned()
            .map(|reference| {
                let occupancy = reference
                    .worktree_path
                    .as_ref()
                    .map(|path| {
                        if prunable_paths.contains(path.as_str()) {
                            Occupancy::MissingWorktree(path.clone())
                        } else {
                            Occupancy::Worktree(path.clone())
                        }
                    })
                    .unwrap_or(Occupancy::Free);
                GuardedRef {
                    reference,
                    occupancy,
                    remote_configured,
                }
            })
            .collect();

        Self {
            refs,
            dirty: snapshot.is_dirty,
            default_branch: snapshot.default_branch.clone(),
            current_branch: snapshot.head_ref.clone(),
            in_progress: snapshot.in_progress_operation.clone(),
            lock_held,
        }
    }
}

pub fn evaluate_guards(input: &GuardInput) -> BTreeMap<String, Vec<BlockedReason>> {
    input
        .refs
        .iter()
        .map(|reference| {
            let mut blocked = Vec::new();
            let is_current =
                input.current_branch.as_deref() == Some(reference.reference.name.as_str());
            if !is_current {
                match &reference.occupancy {
                    Occupancy::Free => {}
                    Occupancy::Worktree(path) => {
                        add_worktree_occupancy_reasons(&mut blocked, path, false);
                    }
                    Occupancy::MissingWorktree(path) => {
                        add_worktree_occupancy_reasons(&mut blocked, path, true);
                    }
                }
            }
            if is_current {
                blocked.push(BlockedReason {
                    operation: "checkout".into(),
                    code: BlockedCode::CurrentBranch.as_str().into(),
                    message: "Already checked out.".into(),
                });
                blocked.push(BlockedReason {
                    operation: "delete-branch".into(),
                    code: BlockedCode::CurrentBranch.as_str().into(),
                    message: "Delete is blocked: this is the current branch.".into(),
                });
            }
            if input.default_branch.as_deref() == Some(reference.reference.name.as_str()) {
                blocked.push(BlockedReason {
                    operation: "delete-branch".into(),
                    code: BlockedCode::DefaultBranch.as_str().into(),
                    message: "Delete is blocked: this is the default branch.".into(),
                });
            }
            if input.dirty {
                for (operation, label) in [
                    ("checkout", "Checkout"),
                    ("merge", "Merge"),
                    ("rebase", "Rebase"),
                ] {
                    blocked.push(BlockedReason {
                        operation: operation.into(),
                        code: BlockedCode::DirtyWorkingTree.as_str().into(),
                        message: format!(
                            "{label} is blocked: the working tree has uncommitted changes."
                        ),
                    });
                }
            }
            if reference.reference.upstream.is_none() {
                blocked.push(BlockedReason {
                    operation: "push".into(),
                    code: BlockedCode::NoUpstream.as_str().into(),
                    message: "Push is blocked: this branch has no upstream.".into(),
                });
            }
            if !reference.remote_configured {
                for (operation, label) in [("push", "Push"), ("fetch", "Fetch"), ("pull", "Pull")] {
                    blocked.push(BlockedReason {
                        operation: operation.into(),
                        code: BlockedCode::NoRemote.as_str().into(),
                        message: format!("{label} is blocked: no remote is configured."),
                    });
                }
            }
            if input.current_branch.is_none() {
                blocked.push(BlockedReason {
                    operation: "commit-to-branch".into(),
                    code: BlockedCode::DetachedHead.as_str().into(),
                    message: "Commit to branch is blocked: HEAD is detached.".into(),
                });
            }
            if let Some(operation) = input.in_progress.as_ref()
                && let Some(label) = in_progress_label(operation.kind)
            {
                for mutating_operation in MUTATING_OPERATIONS
                    .iter()
                    .filter(|operation| !RECOVERY_OPERATIONS.contains(operation))
                {
                    blocked.push(BlockedReason {
                        operation: (*mutating_operation).into(),
                        code: BlockedCode::MergeInProgress.as_str().into(),
                        message: format!(
                            "Blocked: a {label} is in progress; resolve or abort it first."
                        ),
                    });
                }
            }
            if input.lock_held {
                for operation in MUTATING_OPERATIONS {
                    blocked.push(BlockedReason {
                        operation: (*operation).into(),
                        code: BlockedCode::OperationInFlight.as_str().into(),
                        message: "Blocked: another Git Manager operation is already running."
                            .into(),
                    });
                }
            }
            (reference.reference.name.clone(), blocked)
        })
        .collect()
}

fn add_worktree_occupancy_reasons(
    blocked: &mut Vec<BlockedReason>,
    path: &str,
    directory_missing: bool,
) {
    let (
        checkout_message,
        delete_message,
        rename_message,
        move_message,
        update_message,
        rebase_message,
    ) = if directory_missing {
        (
            format!(
                "Checkout is blocked: this branch is held by the worktree registration at {path}, but its directory is missing; remove or prune the worktree first."
            ),
            format!(
                "Delete is blocked: this branch is held by the worktree registration at {path}, but its directory is missing; remove or prune the worktree first."
            ),
            format!(
                "Rename is blocked: the worktree registration at {path} holds this branch, but its directory is missing; remove or prune the worktree first."
            ),
            format!(
                "Cannot move this branch: the worktree registration at {path} still holds it, but its directory is missing; remove or prune the worktree first."
            ),
            format!(
                "Cannot update this branch: the worktree registration at {path} still holds it, but its directory is missing; remove or prune the worktree first."
            ),
            format!(
                "Rebase is blocked: the worktree registration at {path} still holds this branch, but its directory is missing; remove or prune the worktree first."
            ),
        )
    } else {
        (
            format!(
                "Checkout is blocked: this branch is already checked out in the worktree at {path}."
            ),
            format!("Delete is blocked: this branch is checked out in the worktree at {path}."),
            format!("Rename is blocked: the worktree at {path} has this branch checked out."),
            format!("Cannot move this branch: it is checked out in the worktree at {path}."),
            format!("Cannot update this branch: it is checked out in the worktree at {path}."),
            format!("Rebase is blocked: this branch is checked out in the worktree at {path}."),
        )
    };

    blocked.push(blocked_reason(
        "checkout",
        BlockedCode::WorktreeCheckedOut,
        checkout_message,
    ));
    blocked.push(blocked_reason(
        "delete-branch",
        BlockedCode::WorktreeCheckedOut,
        delete_message,
    ));
    // App policy: Git permits this rename and silently retargets the other
    // worktree's HEAD. Coddy Studio blocks it to keep catalog/thread branch names in sync.
    blocked.push(blocked_reason(
        "rename-branch",
        BlockedCode::WorktreeCheckedOut,
        rename_message,
    ));
    for operation in ["force-move", "reset"] {
        blocked.push(blocked_reason(
            operation,
            BlockedCode::WorktreeCheckedOut,
            move_message.clone(),
        ));
    }
    for operation in ["fetch", "pull", "merge-into"] {
        blocked.push(blocked_reason(
            operation,
            BlockedCode::WorktreeCheckedOut,
            update_message.clone(),
        ));
    }
    blocked.push(blocked_reason(
        "rebase",
        BlockedCode::WorktreeCheckedOut,
        rebase_message,
    ));
}

fn blocked_reason(operation: &str, code: BlockedCode, message: impl Into<String>) -> BlockedReason {
    BlockedReason {
        operation: operation.into(),
        code: code.as_str().into(),
        message: message.into(),
    }
}

const fn in_progress_label(kind: GitManagerInProgressKind) -> Option<&'static str> {
    match kind {
        GitManagerInProgressKind::Merge => Some("merge"),
        GitManagerInProgressKind::Rebase => Some("rebase"),
        GitManagerInProgressKind::CherryPick => Some("cherry-pick"),
        GitManagerInProgressKind::Revert | GitManagerInProgressKind::Squash => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::git::manager::refs::GitManagerWorktreeEntry;

    fn branch(name: &str, occupancy: Occupancy) -> GuardedRef {
        GuardedRef {
            reference: GitManagerRefEntry {
                name: name.into(),
                tip_sha: "0123456789012345678901234567890123456789".into(),
                upstream: Some("origin/main".into()),
                ahead: 0,
                behind: 0,
                current: false,
                is_default: false,
                worktree_path: None,
                blocked: Vec::new(),
            },
            occupancy,
            remote_configured: true,
        }
    }

    #[test]
    fn checkout_of_a_branch_held_by_another_worktree_is_blocked() {
        let input = GuardInput {
            refs: vec![branch(
                "feature",
                Occupancy::Worktree("/repo/wt-feature".into()),
            )],
            dirty: false,
            default_branch: Some("main".into()),
            current_branch: Some("main".into()),
            in_progress: None,
            lock_held: false,
        };

        let blocked = evaluate_guards(&input);
        let reason = blocked["feature"]
            .iter()
            .find(|reason| reason.operation == "checkout")
            .expect("checkout is blocked");

        assert_eq!(reason.code, BlockedCode::WorktreeCheckedOut.as_str());
        assert!(reason.message.contains("/repo/wt-feature"));
    }

    #[test]
    fn checkout_of_the_current_branch_is_blocked() {
        let input = GuardInput {
            refs: vec![branch("feature", Occupancy::Free)],
            dirty: false,
            default_branch: Some("main".into()),
            current_branch: Some("feature".into()),
            in_progress: None,
            lock_held: false,
        };

        let blocked = evaluate_guards(&input);
        let reason = blocked["feature"]
            .iter()
            .find(|reason| reason.operation == "checkout")
            .expect("checkout is blocked");

        assert_eq!(reason.code, BlockedCode::CurrentBranch.as_str());
        assert_eq!(reason.message, "Already checked out.");
    }

    #[test]
    fn dirty_working_tree_blocks_checkout_merge_and_rebase() {
        let input = GuardInput {
            refs: vec![branch("feature", Occupancy::Free)],
            dirty: true,
            default_branch: Some("main".into()),
            current_branch: Some("main".into()),
            in_progress: None,
            lock_held: false,
        };

        let blocked = evaluate_guards(&input);
        for operation in ["checkout", "merge", "rebase"] {
            let reason = blocked["feature"]
                .iter()
                .find(|reason| reason.operation == operation)
                .unwrap_or_else(|| panic!("{operation} is blocked"));
            assert_eq!(reason.code, BlockedCode::DirtyWorkingTree.as_str());
            assert!(reason.message.contains("uncommitted changes"));
        }
    }

    #[test]
    fn delete_of_a_branch_held_by_a_worktree_is_blocked() {
        let input = GuardInput {
            refs: vec![branch(
                "feature",
                Occupancy::Worktree("/repo/wt-feature".into()),
            )],
            dirty: false,
            default_branch: Some("main".into()),
            current_branch: Some("main".into()),
            in_progress: None,
            lock_held: false,
        };

        let blocked = evaluate_guards(&input);
        let reason = blocked["feature"]
            .iter()
            .find(|reason| reason.operation == "delete-branch")
            .expect("delete is blocked");

        assert_eq!(reason.code, BlockedCode::WorktreeCheckedOut.as_str());
        assert!(reason.message.contains("/repo/wt-feature"));
    }

    #[test]
    fn delete_of_a_branch_with_a_missing_worktree_directory_requires_pruning() {
        let input = GuardInput {
            refs: vec![branch(
                "feature",
                Occupancy::MissingWorktree("/repo/wt-feature".into()),
            )],
            dirty: false,
            default_branch: Some("main".into()),
            current_branch: Some("main".into()),
            in_progress: None,
            lock_held: false,
        };

        let blocked = evaluate_guards(&input);
        let reason = blocked["feature"]
            .iter()
            .find(|reason| reason.operation == "delete-branch")
            .expect("delete is blocked");

        assert_eq!(reason.code, BlockedCode::WorktreeCheckedOut.as_str());
        assert!(reason.message.contains("/repo/wt-feature"));
        assert!(reason.message.contains("directory is missing"));
        assert!(
            reason
                .message
                .contains("remove or prune the worktree first")
        );
    }

    #[test]
    fn delete_of_the_current_branch_is_blocked() {
        let input = GuardInput {
            refs: vec![branch("feature", Occupancy::Free)],
            dirty: false,
            default_branch: Some("main".into()),
            current_branch: Some("feature".into()),
            in_progress: None,
            lock_held: false,
        };

        let blocked = evaluate_guards(&input);
        let reason = blocked["feature"]
            .iter()
            .find(|reason| reason.operation == "delete-branch")
            .expect("delete is blocked");

        assert_eq!(reason.code, BlockedCode::CurrentBranch.as_str());
        assert!(reason.message.contains("current branch"));
    }

    #[test]
    fn delete_of_the_default_branch_is_blocked() {
        let input = GuardInput {
            refs: vec![branch("main", Occupancy::Free)],
            dirty: false,
            default_branch: Some("main".into()),
            current_branch: Some("feature".into()),
            in_progress: None,
            lock_held: false,
        };

        let blocked = evaluate_guards(&input);
        let reason = blocked["main"]
            .iter()
            .find(|reason| reason.operation == "delete-branch")
            .expect("delete is blocked");

        assert_eq!(reason.code, BlockedCode::DefaultBranch.as_str());
        assert!(reason.message.contains("default branch"));
    }

    #[test]
    fn rename_of_a_branch_held_by_another_worktree_is_blocked_by_app_policy() {
        let input = GuardInput {
            refs: vec![branch(
                "feature",
                Occupancy::Worktree("/repo/wt-feature".into()),
            )],
            dirty: false,
            default_branch: Some("main".into()),
            current_branch: Some("main".into()),
            in_progress: None,
            lock_held: false,
        };

        let blocked = evaluate_guards(&input);
        let reason = blocked["feature"]
            .iter()
            .find(|reason| reason.operation == "rename-branch")
            .expect("rename is blocked");

        assert_eq!(reason.code, BlockedCode::WorktreeCheckedOut.as_str());
        assert!(reason.message.contains("/repo/wt-feature"));
    }

    #[test]
    fn force_move_and_reset_of_a_held_branch_are_blocked() {
        let input = GuardInput {
            refs: vec![branch(
                "feature",
                Occupancy::Worktree("/repo/wt-feature".into()),
            )],
            dirty: false,
            default_branch: Some("main".into()),
            current_branch: Some("main".into()),
            in_progress: None,
            lock_held: false,
        };

        let blocked = evaluate_guards(&input);
        for operation in ["force-move", "reset"] {
            let reason = blocked["feature"]
                .iter()
                .find(|reason| reason.operation == operation)
                .unwrap_or_else(|| panic!("{operation} is blocked"));
            assert_eq!(reason.code, BlockedCode::WorktreeCheckedOut.as_str());
            assert!(reason.message.contains("/repo/wt-feature"));
        }
    }

    #[test]
    fn fetch_and_pull_into_a_held_destination_branch_are_blocked() {
        let input = GuardInput {
            refs: vec![branch(
                "feature",
                Occupancy::Worktree("/repo/wt-feature".into()),
            )],
            dirty: false,
            default_branch: Some("main".into()),
            current_branch: Some("main".into()),
            in_progress: None,
            lock_held: false,
        };

        let blocked = evaluate_guards(&input);
        for operation in ["fetch", "pull"] {
            let reason = blocked["feature"]
                .iter()
                .find(|reason| reason.operation == operation)
                .unwrap_or_else(|| panic!("{operation} is blocked"));
            assert_eq!(reason.code, BlockedCode::WorktreeCheckedOut.as_str());
            assert!(reason.message.contains("/repo/wt-feature"));
        }
    }

    #[test]
    fn rebase_of_a_branch_held_by_another_worktree_is_blocked() {
        let input = GuardInput {
            refs: vec![branch(
                "feature",
                Occupancy::Worktree("/repo/wt-feature".into()),
            )],
            dirty: false,
            default_branch: Some("main".into()),
            current_branch: Some("main".into()),
            in_progress: None,
            lock_held: false,
        };

        let blocked = evaluate_guards(&input);
        let reason = blocked["feature"]
            .iter()
            .find(|reason| reason.operation == "rebase")
            .expect("rebase is blocked");

        assert_eq!(reason.code, BlockedCode::WorktreeCheckedOut.as_str());
        assert!(reason.message.contains("/repo/wt-feature"));
    }

    #[test]
    fn repository_lock_blocks_every_mutating_operation() {
        let input = GuardInput {
            refs: vec![branch("feature", Occupancy::Free)],
            dirty: false,
            default_branch: Some("main".into()),
            current_branch: Some("main".into()),
            in_progress: None,
            lock_held: true,
        };

        let blocked = evaluate_guards(&input);
        let reasons = blocked["feature"]
            .iter()
            .filter(|reason| reason.code == BlockedCode::OperationInFlight.as_str())
            .collect::<Vec<_>>();
        let operations = reasons
            .iter()
            .map(|reason| reason.operation.as_str())
            .collect::<BTreeSet<_>>();
        let expected = [
            "abort",
            "branch-checkout",
            "branch-create",
            "branch-delete",
            "branch-rename",
            "checkout",
            "cherry-pick",
            "commit",
            "commit-to-branch",
            "continue",
            "delete-branch",
            "discard",
            "discard-partial",
            "fetch",
            "force-move",
            "force-push",
            "merge",
            "merge-into",
            "publish-branch",
            "pull",
            "push",
            "rebase",
            "rename-branch",
            "reorder",
            "reset",
            "resolve-conflict",
            "revert",
            "squash",
            "squash-merge",
            "stage-partial",
            "stash-apply",
            "stash-drop",
            "stash-pop",
            "stash-push",
            "tag-create",
            "tag-delete",
            "tag-push",
            "undo-commit",
            "unstage-partial",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();

        assert_eq!(operations, expected);
        assert!(
            reasons
                .iter()
                .all(|reason| reason.message.contains("operation is already running"))
        );
    }

    #[test]
    fn pending_merge_rebase_or_cherry_pick_blocks_mutations_except_recovery_paths() {
        let expected = [
            "branch-checkout",
            "branch-create",
            "branch-delete",
            "branch-rename",
            "checkout",
            "cherry-pick",
            "commit",
            "commit-to-branch",
            "delete-branch",
            "discard",
            "discard-partial",
            "fetch",
            "force-move",
            "force-push",
            "merge",
            "merge-into",
            "publish-branch",
            "pull",
            "push",
            "rebase",
            "rename-branch",
            "reorder",
            "reset",
            "revert",
            "squash",
            "squash-merge",
            "stage-partial",
            "stash-apply",
            "stash-drop",
            "stash-pop",
            "stash-push",
            "tag-create",
            "tag-delete",
            "tag-push",
            "undo-commit",
            "unstage-partial",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();

        for (kind, label) in [
            (GitManagerInProgressKind::Merge, "merge"),
            (GitManagerInProgressKind::Rebase, "rebase"),
            (GitManagerInProgressKind::CherryPick, "cherry-pick"),
        ] {
            let input = GuardInput {
                refs: vec![branch("feature", Occupancy::Free)],
                dirty: true,
                default_branch: Some("main".into()),
                current_branch: Some("main".into()),
                in_progress: Some(GitManagerInProgressOperation {
                    kind,
                    current: None,
                    total: None,
                }),
                lock_held: false,
            };

            let blocked = evaluate_guards(&input);
            let reasons = blocked["feature"]
                .iter()
                .filter(|reason| reason.code == BlockedCode::MergeInProgress.as_str())
                .collect::<Vec<_>>();
            let operations = reasons
                .iter()
                .map(|reason| reason.operation.as_str())
                .collect::<BTreeSet<_>>();

            assert_eq!(operations, expected, "pending {label}");
            assert!(reasons.iter().all(|reason| reason.message.contains(label)));
            for recovery_operation in ["resolve-conflict", "continue", "abort"] {
                assert!(
                    blocked["feature"]
                        .iter()
                        .all(|reason| reason.operation != recovery_operation),
                    "{recovery_operation} is exempt from dirty and in-progress guards"
                );
            }
        }
    }

    #[test]
    fn push_without_an_upstream_is_blocked() {
        let mut reference = branch("feature", Occupancy::Free);
        reference.reference.upstream = None;
        let input = GuardInput {
            refs: vec![reference],
            dirty: false,
            default_branch: Some("main".into()),
            current_branch: Some("main".into()),
            in_progress: None,
            lock_held: false,
        };

        let blocked = evaluate_guards(&input);
        let reason = blocked["feature"]
            .iter()
            .find(|reason| reason.operation == "push")
            .expect("push is blocked");

        assert_eq!(reason.code, BlockedCode::NoUpstream.as_str());
        assert!(reason.message.contains("no upstream"));
    }

    #[test]
    fn network_operations_without_a_configured_remote_are_blocked() {
        let mut reference = branch("feature", Occupancy::Free);
        reference.remote_configured = false;
        let input = GuardInput {
            refs: vec![reference],
            dirty: false,
            default_branch: Some("main".into()),
            current_branch: Some("main".into()),
            in_progress: None,
            lock_held: false,
        };

        let blocked = evaluate_guards(&input);
        for operation in ["push", "fetch", "pull"] {
            let reason = blocked["feature"]
                .iter()
                .find(|reason| reason.operation == operation)
                .unwrap_or_else(|| panic!("{operation} is blocked"));
            assert_eq!(reason.code, BlockedCode::NoRemote.as_str());
            assert!(reason.message.contains("no remote is configured"));
        }
    }

    #[test]
    fn branch_commit_while_head_is_detached_is_blocked() {
        let input = GuardInput {
            refs: vec![branch("feature", Occupancy::Free)],
            dirty: false,
            default_branch: Some("main".into()),
            current_branch: None,
            in_progress: None,
            lock_held: false,
        };

        let blocked = evaluate_guards(&input);
        let reason = blocked["feature"]
            .iter()
            .find(|reason| reason.operation == "commit-to-branch")
            .expect("branch commit is blocked");

        assert_eq!(reason.code, BlockedCode::DetachedHead.as_str());
        assert!(reason.message.contains("HEAD is detached"));
    }

    #[test]
    fn clean_non_current_branch_with_upstream_has_no_blocked_reasons() {
        let input = GuardInput {
            refs: vec![branch("feature", Occupancy::Free)],
            dirty: false,
            default_branch: Some("main".into()),
            current_branch: Some("main".into()),
            in_progress: None,
            lock_held: false,
        };

        let blocked = evaluate_guards(&input);

        assert!(blocked["feature"].is_empty());
    }

    #[test]
    fn snapshot_adapter_preserves_registered_missing_worktree_occupancy() {
        let mut reference = branch("feature", Occupancy::Free).reference;
        reference.worktree_path = Some("/repo/wt-feature".into());
        let snapshot = GitManagerRefsSnapshot {
            generation: 1,
            head_ref: Some("main".into()),
            detached_sha: None,
            is_dirty: false,
            default_branch: Some("main".into()),
            remotes: vec!["origin".into()],
            local_branches: vec![reference],
            remote_branches: Vec::new(),
            tags: Vec::new(),
            worktrees: vec![GitManagerWorktreeEntry {
                path: "/repo/wt-feature".into(),
                head_sha: "0123456789012345678901234567890123456789".into(),
                branch: Some("feature".into()),
                is_primary: false,
                is_bare: false,
                is_detached: false,
                locked: false,
                lock_reason: None,
                prunable: true,
            }],
            in_progress_operation: None,
            conflicted_paths: Vec::new(),
        };

        let input = GuardInput::from_snapshot(&snapshot, false);

        assert_eq!(
            input.refs[0].occupancy,
            Occupancy::MissingWorktree("/repo/wt-feature".into())
        );
        assert_eq!(input.current_branch.as_deref(), Some("main"));
        assert_eq!(input.default_branch.as_deref(), Some("main"));
        assert!(!input.dirty);
        assert!(!input.lock_held);
    }

    #[test]
    fn missing_worktree_directory_still_holds_the_branch_for_all_occupancy_guards() {
        let input = GuardInput {
            refs: vec![branch(
                "feature",
                Occupancy::MissingWorktree("/repo/wt-feature".into()),
            )],
            dirty: false,
            default_branch: Some("main".into()),
            current_branch: Some("main".into()),
            in_progress: None,
            lock_held: false,
        };

        let blocked = evaluate_guards(&input);
        let reasons = blocked["feature"]
            .iter()
            .filter(|reason| reason.code == BlockedCode::WorktreeCheckedOut.as_str())
            .collect::<Vec<_>>();
        let operations = reasons
            .iter()
            .map(|reason| reason.operation.as_str())
            .collect::<BTreeSet<_>>();

        assert_eq!(
            operations,
            [
                "checkout",
                "delete-branch",
                "fetch",
                "force-move",
                "merge-into",
                "pull",
                "rebase",
                "rename-branch",
                "reset",
            ]
            .into_iter()
            .collect()
        );
        assert!(
            reasons
                .iter()
                .all(|reason| reason.message.contains("/repo/wt-feature"))
        );
    }
}
