use serde_json::{Value, json};

use super::model::{
    SCOPE_ACCESS_READ, SCOPE_ACCESS_WRITE, SCOPE_ORCHESTRATION_OPERATE, SCOPE_ORCHESTRATION_READ,
    SCOPE_RELAY_WRITE, SCOPE_REVIEW_WRITE, SCOPE_TERMINAL_OPERATE,
};

pub(crate) const ACTIVITY_READ_SCOPE: &str = SCOPE_ORCHESTRATION_READ;

#[must_use]
pub(crate) fn required_scope(method: &str) -> Option<&'static str> {
    if matches!(
        method,
        "activity.getSnapshot"
            | "activity.listDetail"
            | "activity.listRoster"
            | "subscribeActivity"
    ) {
        return Some(ACTIVITY_READ_SCOPE);
    }
    match method {
        "assets.createUrl"
        | "filesystem.browse"
        | "gitManager.getCommits"
        | "gitManager.getDiff"
        | "gitManager.getRefs"
        | "gitManager.getRemoteTags"
        | "gitManager.getStashes"
        | "gitManager.listPullRequests"
        | "gitManager.previewMerge"
        | "pullRequests.getContext"
        | "pullRequests.getVocabulary"
        | "pullRequests.getCreateDefaults"
        | "pullRequests.list"
        | "pullRequests.get"
        | "pullRequests.getTimeline"
        | "pullRequests.getCommits"
        | "pullRequests.getChecks"
        | "pullRequests.getFiles"
        | "pullRequests.readSnapshot"
        | "pullRequests.subscribe"
        | "orchestration.getArchivedShellSnapshot"
        | "orchestration.getFullThreadDiff"
        | "orchestration.getTurnDiff"
        | "orchestration.replayEvents"
        | "orchestration.subscribeShell"
        | "orchestration.subscribeThread"
        | "preview.list"
        | "projects.createDownloadUrl"
        | "projects.listEntries"
        | "projects.readFile"
        | "projects.searchEntries"
        | "server.discoverSourceControl"
        | "server.getConfig"
        | "server.getProcessDiagnostics"
        | "server.getProcessResourceHistory"
        | "server.getProfilerStatus"
        | "server.getProviderCapabilities"
        | "server.getProviderUsage"
        | "server.getSettings"
        | "server.getTraceDiagnostics"
        | "updater.activeWork"
        | "updater.status"
        | "sourceControl.lookupRepository"
        | "subscribeDiscoveredLocalServers"
        | "subscribeGitManagerSignal"
        | "subscribePreviewEvents"
        | "subscribeProjectEntries"
        | "subscribeServerConfig"
        | "subscribeServerLifecycle"
        | "subscribeVcsStatus"
        | "subscribeVcsStatusSummary"
        | "subscribeWorktreeCatalog"
        | "vcs.listCommits"
        | "vcs.listRefs"
        | "vcs.refreshStatus"
        | "vcs.refreshWorktreeCatalog"
        | "worktree.getRemovalPlan" => Some(SCOPE_ORCHESTRATION_READ),
        // Scan reads CLI transcripts (first prompts included) from the server host's home
        // directory, outside BiBCode's data, so read-only credentials must not see them.
        "agentSessions.import"
        | "agentSessions.scan"
        | "git.preparePullRequestThread"
        | "git.resolvePullRequest"
        | "git.runStackedAction"
        | "gitManager.commit"
        | "gitManager.discard"
        | "gitManager.discardPartial"
        | "gitManager.runOperation"
        | "gitManager.stagePartial"
        | "gitManager.undoCommit"
        | "gitManager.unstagePartial"
        | "pullRequests.runAction"
        | "pullRequests.checkout"
        | "activity.cancelSubtree"
        | "activity.retrySubtreeCancellation"
        | "uploads.begin"
        | "uploads.append"
        | "uploads.get"
        | "uploads.cancel"
        | "orchestration.dispatchCommand"
        | "preview.claimOpenRequest"
        | "preview.close"
        | "preview.gatewayOpen"
        | "preview.tunnelClose"
        | "preview.tunnelOpen"
        | "preview.tunnelWrite"
        | "preview.navigate"
        | "preview.open"
        | "preview.refresh"
        | "preview.reportStatus"
        | "preview.resize"
        | "previewAutomation.connect"
        | "previewAutomation.focusHost"
        | "previewAutomation.respond"
        | "projects.createEntry"
        | "projects.createUploadUrl"
        | "projects.deleteEntry"
        | "projects.duplicateEntry"
        | "projects.renameEntry"
        | "projects.writeFile"
        | "server.consumeCodexRateLimitReset"
        | "server.refreshProviders"
        | "server.refreshProviderUsage"
        | "server.removeKeybinding"
        | "server.reportProfilerClientBatch"
        | "server.signalProcess"
        | "server.updateProvider"
        | "server.updateSettings"
        | "server.upsertKeybinding"
        | "updater.check"
        | "updater.install"
        | "shell.openInEditor"
        | "sourceControl.cloneRepository"
        | "sourceControl.publishRepository"
        | "vcs.cancelClone"
        | "vcs.clone"
        | "vcs.createRef"
        | "vcs.discardFiles"
        | "vcs.generateCommitMessage"
        | "vcs.init"
        | "vcs.pull"
        | "vcs.stageFiles"
        | "vcs.switchRef"
        | "vcs.unstageFiles" => Some(SCOPE_ORCHESTRATION_OPERATE),
        "worktree.adopt"
        | "worktree.createManaged"
        | "worktree.createPanel"
        | "worktree.remove"
        | "worktree.removeFromBibCode"
        | "worktree.retarget"
        | "worktree.updateDiscoveryPolicy" => Some(SCOPE_ORCHESTRATION_OPERATE),
        "terminal.attach"
        | "terminal.beginInput"
        | "terminal.cancelInput"
        | "terminal.clear"
        | "terminal.close"
        | "terminal.open"
        | "terminal.resize"
        | "terminal.restart"
        | "terminal.stageImagePaste"
        | "terminal.write"
        | "terminal.writeInput"
        | "subscribeTerminalEvents"
        | "subscribeTerminalMetadata" => Some(SCOPE_TERMINAL_OPERATE),
        "review.getDiffPreview" => Some(SCOPE_REVIEW_WRITE),
        "cloud.getRelayClientStatus" | "cloud.installRelayClient" => Some(SCOPE_RELAY_WRITE),
        "subscribeAuthAccess" => Some(SCOPE_ACCESS_READ),
        // Two-tier policy: a pending-pairing session confirms its OWN
        // delivery through the session capability gate (which bypasses this
        // scope, since standard device grants do not carry access scopes);
        // any other caller needs access:write. Locked from both directions by
        // confirm_pairing_policy_is_pending_capability_or_access_write and
        // the e2ee scope-bypass suite.
        "auth.confirmPairing" => Some(SCOPE_ACCESS_WRITE),
        _ => None,
    }
}

#[must_use]
pub(crate) fn authorization_error(required_scope: &str) -> Value {
    json!({
        "_tag": "EnvironmentAuthorizationError",
        "message": format!(
            "The authenticated token is missing required scope: {required_scope}."
        ),
        "requiredScope": required_scope,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::ACTIVE_RPC_METHODS;

    #[test]
    fn every_active_rpc_method_has_exactly_one_declared_scope() {
        let missing = ACTIVE_RPC_METHODS
            .iter()
            .filter(|method| required_scope(method.name).is_none())
            .map(|method| method.name)
            .collect::<Vec<_>>();

        assert!(missing.is_empty(), "missing RPC scopes: {missing:?}");
        assert_eq!(
            required_scope("server.getConfig"),
            Some(SCOPE_ORCHESTRATION_READ)
        );
        assert_eq!(
            required_scope("server.getProviderCapabilities"),
            Some(SCOPE_ORCHESTRATION_READ)
        );
        assert_eq!(
            required_scope("server.updateSettings"),
            Some(SCOPE_ORCHESTRATION_OPERATE)
        );
        assert_eq!(
            required_scope("agentSessions.scan"),
            Some(SCOPE_ORCHESTRATION_OPERATE)
        );
        assert_eq!(
            required_scope("agentSessions.import"),
            Some(SCOPE_ORCHESTRATION_OPERATE)
        );
        assert_eq!(
            required_scope("server.consumeCodexRateLimitReset"),
            Some(SCOPE_ORCHESTRATION_OPERATE)
        );
        assert_eq!(
            required_scope("server.getProfilerStatus"),
            Some(SCOPE_ORCHESTRATION_READ)
        );
        assert_eq!(
            required_scope("server.reportProfilerClientBatch"),
            Some(SCOPE_ORCHESTRATION_OPERATE)
        );
        assert_eq!(
            required_scope("subscribeAuthAccess"),
            Some(SCOPE_ACCESS_READ)
        );
        assert_eq!(
            required_scope("auth.confirmPairing"),
            Some(SCOPE_ACCESS_WRITE)
        );
        assert_eq!(
            required_scope("updater.status"),
            Some(SCOPE_ORCHESTRATION_READ)
        );
        assert_eq!(
            required_scope("updater.activeWork"),
            Some(SCOPE_ORCHESTRATION_READ)
        );
        for method in ["updater.check", "updater.install"] {
            assert_eq!(
                required_scope(method),
                Some(SCOPE_ORCHESTRATION_OPERATE),
                "wrong updater scope for {method}"
            );
        }
        for method in [
            "activity.getSnapshot",
            "activity.listDetail",
            "activity.listRoster",
            "subscribeActivity",
        ] {
            assert_eq!(
                required_scope(method),
                Some(ACTIVITY_READ_SCOPE),
                "wrong activity RPC scope for {method}"
            );
        }
        for method in [
            "pullRequests.getContext",
            "pullRequests.getVocabulary",
            "pullRequests.getCreateDefaults",
            "pullRequests.list",
            "pullRequests.get",
            "pullRequests.getTimeline",
            "pullRequests.getCommits",
            "pullRequests.getChecks",
            "pullRequests.getFiles",
            "pullRequests.readSnapshot",
            "pullRequests.subscribe",
            "gitManager.getCommits",
            "gitManager.getDiff",
            "gitManager.getRefs",
            "gitManager.getRemoteTags",
            "gitManager.getStashes",
            "gitManager.listPullRequests",
            "gitManager.previewMerge",
            "subscribeGitManagerSignal",
            "subscribeVcsStatusSummary",
            "subscribeWorktreeCatalog",
            "vcs.refreshWorktreeCatalog",
            "worktree.getRemovalPlan",
        ] {
            assert_eq!(
                required_scope(method),
                Some(SCOPE_ORCHESTRATION_READ),
                "wrong worktree catalog read scope for {method}"
            );
        }
        assert_eq!(
            required_scope("worktree.updateDiscoveryPolicy"),
            Some(SCOPE_ORCHESTRATION_OPERATE)
        );
        assert_eq!(
            required_scope("worktree.adopt"),
            Some(SCOPE_ORCHESTRATION_OPERATE)
        );
        for method in [
            "pullRequests.runAction",
            "pullRequests.checkout",
            "gitManager.commit",
            "gitManager.discard",
            "gitManager.discardPartial",
            "gitManager.runOperation",
            "gitManager.stagePartial",
            "gitManager.undoCommit",
            "gitManager.unstagePartial",
        ] {
            assert_eq!(
                required_scope(method),
                Some(SCOPE_ORCHESTRATION_OPERATE),
                "wrong Git Manager mutation scope for {method}"
            );
        }
        for method in ["worktree.remove", "worktree.removeFromBibCode"] {
            assert_eq!(required_scope(method), Some(SCOPE_ORCHESTRATION_OPERATE));
        }
        for method in [
            "activity.cancelSubtree",
            "activity.retrySubtreeCancellation",
        ] {
            assert_eq!(
                required_scope(method),
                Some(SCOPE_ORCHESTRATION_OPERATE),
                "wrong activity mutation scope for {method}"
            );
        }
        assert_eq!(
            required_scope("preview.gatewayOpen"),
            Some(SCOPE_ORCHESTRATION_OPERATE),
            "a gateway gives read/write access to the upstream, so reading is not enough"
        );
        for method in [
            "preview.tunnelOpen",
            "preview.tunnelWrite",
            "preview.tunnelClose",
        ] {
            assert_eq!(
                required_scope(method),
                Some(SCOPE_ORCHESTRATION_OPERATE),
                "a tunnel reaches the server's loopback like the gateway: {method}"
            );
        }
        assert_eq!(
            required_scope("vcs.cancelClone"),
            Some(SCOPE_ORCHESTRATION_OPERATE),
            "cancelling a clone needs the same scope as starting one"
        );
        assert_eq!(required_scope("unknown.method"), None);
    }
    #[test]
    fn staged_uploads_all_require_orchestration_operate() {
        for method in [
            "uploads.begin",
            "uploads.append",
            "uploads.get",
            "uploads.cancel",
        ] {
            assert_eq!(required_scope(method), Some(SCOPE_ORCHESTRATION_OPERATE));
        }
    }
}
