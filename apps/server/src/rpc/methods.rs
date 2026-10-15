use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MethodMode {
    Stream,
    Unary,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum MethodMutability {
    Read,
    #[default]
    Mutation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RpcMethodSpec {
    pub name: &'static str,
    pub mode: MethodMode,
    pub(crate) mutability: MethodMutability,
}

const fn read_unary(name: &'static str) -> RpcMethodSpec {
    RpcMethodSpec {
        name,
        mode: MethodMode::Unary,
        mutability: MethodMutability::Read,
    }
}

const fn mutation_unary(name: &'static str) -> RpcMethodSpec {
    RpcMethodSpec {
        name,
        mode: MethodMode::Unary,
        mutability: MethodMutability::Mutation,
    }
}

const fn read_stream(name: &'static str) -> RpcMethodSpec {
    RpcMethodSpec {
        name,
        mode: MethodMode::Stream,
        mutability: MethodMutability::Read,
    }
}

const fn mutation_stream(name: &'static str) -> RpcMethodSpec {
    RpcMethodSpec {
        name,
        mode: MethodMode::Stream,
        mutability: MethodMutability::Mutation,
    }
}

pub const ACTIVE_RPC_METHODS: &[RpcMethodSpec] = &[
    mutation_unary("activity.cancelSubtree"),
    read_unary("activity.getSnapshot"),
    read_unary("activity.listDetail"),
    read_unary("activity.listRoster"),
    mutation_unary("activity.retrySubtreeCancellation"),
    mutation_unary("agentSessions.import"),
    read_unary("agentSessions.scan"),
    read_unary("assets.createUrl"),
    mutation_unary("auth.confirmPairing"),
    read_unary("cloud.getRelayClientStatus"),
    mutation_stream("cloud.installRelayClient"),
    mutation_unary("device.close"),
    read_unary("device.createMediaTicket"),
    mutation_unary("device.input"),
    mutation_unary("device.open"),
    mutation_unary("device.rotate"),
    read_stream("device.subscribeCatalog"),
    read_stream("device.subscribeFrames"),
    read_unary("filesystem.browse"),
    mutation_unary("git.preparePullRequestThread"),
    mutation_unary("git.resolvePullRequest"),
    mutation_stream("git.runStackedAction"),
    mutation_unary("gitManager.commit"),
    mutation_unary("gitManager.discard"),
    mutation_unary("gitManager.discardPartial"),
    read_unary("gitManager.getCommits"),
    read_unary("gitManager.getDiff"),
    read_unary("gitManager.getRefs"),
    read_unary("gitManager.getRemoteTags"),
    read_unary("gitManager.getStashes"),
    read_unary("gitManager.listPullRequests"),
    read_unary("gitManager.previewMerge"),
    mutation_stream("gitManager.runOperation"),
    mutation_unary("gitManager.stagePartial"),
    mutation_unary("gitManager.undoCommit"),
    mutation_unary("gitManager.unstagePartial"),
    mutation_unary("orchestration.dispatchCommand"),
    read_unary("orchestration.getArchivedShellSnapshot"),
    read_unary("orchestration.getFullThreadDiff"),
    read_unary("orchestration.getTurnDiff"),
    read_unary("orchestration.replayEvents"),
    read_stream("orchestration.subscribeShell"),
    read_stream("orchestration.subscribeThread"),
    mutation_unary("preview.claimOpenRequest"),
    mutation_unary("preview.close"),
    mutation_unary("preview.gatewayOpen"),
    read_unary("preview.list"),
    mutation_unary("preview.navigate"),
    mutation_unary("preview.open"),
    mutation_unary("preview.refresh"),
    mutation_unary("preview.reportStatus"),
    mutation_unary("preview.resize"),
    // Tunnel traffic changes nothing BiBCode keeps, and a tunnel lives as long as its page:
    // like `previewAutomation.connect`, it must neither hold nor wait for a mutation permit.
    read_unary("preview.tunnelClose"),
    read_stream("preview.tunnelOpen"),
    read_unary("preview.tunnelWrite"),
    // The desktop shell holds this for the window lifetime. It only registers an
    // in-memory automation host, so a mutation permit would make update drain
    // time out and the installer would never start.
    read_stream("previewAutomation.connect"),
    mutation_unary("previewAutomation.focusHost"),
    mutation_unary("previewAutomation.respond"),
    read_unary("projects.createDownloadUrl"),
    mutation_unary("projects.createEntry"),
    mutation_unary("projects.createUploadUrl"),
    mutation_unary("projects.deleteEntry"),
    mutation_unary("projects.duplicateEntry"),
    read_unary("projects.listEntries"),
    read_unary("projects.readFile"),
    mutation_unary("projects.renameEntry"),
    read_unary("projects.searchEntries"),
    mutation_unary("projects.writeFile"),
    mutation_unary("pullRequests.checkout"),
    read_unary("pullRequests.get"),
    read_unary("pullRequests.getChecks"),
    read_unary("pullRequests.getCommits"),
    read_unary("pullRequests.getContext"),
    read_unary("pullRequests.getCreateDefaults"),
    read_unary("pullRequests.getFiles"),
    read_unary("pullRequests.getTimeline"),
    read_unary("pullRequests.getVocabulary"),
    read_unary("pullRequests.list"),
    read_unary("pullRequests.readSnapshot"),
    mutation_unary("pullRequests.runAction"),
    read_stream("pullRequests.subscribe"),
    read_unary("review.getDiffPreview"),
    mutation_unary("server.consumeCodexRateLimitReset"),
    read_unary("server.discoverSourceControl"),
    read_unary("server.getConfig"),
    read_unary("server.getProcessDiagnostics"),
    read_unary("server.getProcessResourceHistory"),
    read_unary("server.getProfilerStatus"),
    read_unary("server.getProviderCapabilities"),
    read_unary("server.getProviderUsage"),
    read_unary("server.getSettings"),
    read_unary("server.getTraceDiagnostics"),
    mutation_unary("server.refreshProviders"),
    mutation_unary("server.refreshProviderUsage"),
    mutation_unary("server.removeKeybinding"),
    mutation_unary("server.reportProfilerClientBatch"),
    mutation_unary("server.signalProcess"),
    mutation_unary("server.updateProvider"),
    mutation_unary("server.updateSettings"),
    mutation_unary("server.upsertKeybinding"),
    mutation_unary("shell.openInEditor"),
    mutation_unary("sourceControl.cloneRepository"),
    read_unary("sourceControl.lookupRepository"),
    mutation_unary("sourceControl.publishRepository"),
    read_stream("subscribeActivity"),
    read_stream("subscribeAuthAccess"),
    read_stream("subscribeDiscoveredLocalServers"),
    read_stream("subscribeGitManagerSignal"),
    read_stream("subscribePreviewEvents"),
    read_stream("subscribeProjectEntries"),
    read_stream("subscribeServerConfig"),
    read_stream("subscribeServerLifecycle"),
    read_stream("subscribeTerminalEvents"),
    read_stream("subscribeTerminalMetadata"),
    read_stream("subscribeVcsStatus"),
    read_stream("subscribeVcsStatusSummary"),
    read_stream("subscribeWorktreeCatalog"),
    mutation_stream("terminal.attach"),
    mutation_unary("terminal.beginInput"),
    mutation_unary("terminal.cancelInput"),
    mutation_unary("terminal.clear"),
    mutation_unary("terminal.close"),
    mutation_unary("terminal.open"),
    mutation_unary("terminal.resize"),
    mutation_unary("terminal.restart"),
    mutation_unary("terminal.stageImagePaste"),
    mutation_unary("terminal.write"),
    mutation_unary("terminal.writeInput"),
    read_unary("updater.activeWork"),
    mutation_unary("updater.check"),
    mutation_unary("updater.install"),
    read_unary("updater.status"),
    mutation_unary("uploads.append"),
    mutation_unary("uploads.begin"),
    mutation_unary("uploads.cancel"),
    read_unary("uploads.get"),
    mutation_unary("vcs.cancelClone"),
    mutation_unary("vcs.clone"),
    mutation_unary("vcs.createRef"),
    mutation_unary("vcs.discardFiles"),
    mutation_unary("vcs.generateCommitMessage"),
    mutation_unary("vcs.init"),
    read_unary("vcs.listCommits"),
    read_unary("vcs.listRefs"),
    mutation_unary("vcs.pull"),
    read_unary("vcs.refreshStatus"),
    read_unary("vcs.refreshWorktreeCatalog"),
    mutation_unary("vcs.stageFiles"),
    mutation_unary("vcs.switchRef"),
    mutation_unary("vcs.unstageFiles"),
    mutation_unary("worktree.adopt"),
    mutation_unary("worktree.createManaged"),
    mutation_unary("worktree.createPanel"),
    read_unary("worktree.getRemovalPlan"),
    mutation_unary("worktree.remove"),
    mutation_unary("worktree.removeFromBibCode"),
    mutation_unary("worktree.retarget"),
    mutation_unary("worktree.updateDiscoveryPolicy"),
];

#[must_use]
pub(crate) fn method_mutability(name: &str) -> Option<MethodMutability> {
    ACTIVE_RPC_METHODS
        .iter()
        .find(|method| method.name == name)
        .map(|method| method.mutability)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_requests_methods_are_unary_with_explicit_mutability() {
        for name in [
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
        ] {
            assert!(ACTIVE_RPC_METHODS.contains(&read_unary(name)), "{name}");
        }
        assert!(ACTIVE_RPC_METHODS.contains(&read_stream("pullRequests.subscribe")));
        for name in ["pullRequests.runAction", "pullRequests.checkout"] {
            assert!(ACTIVE_RPC_METHODS.contains(&mutation_unary(name)), "{name}");
        }
    }

    #[test]
    fn device_methods_have_their_modes() {
        for name in ["device.subscribeCatalog", "device.subscribeFrames"] {
            assert!(ACTIVE_RPC_METHODS.contains(&read_stream(name)), "{name}");
        }
        for name in [
            "device.open",
            "device.close",
            "device.input",
            "device.rotate",
        ] {
            assert!(ACTIVE_RPC_METHODS.contains(&mutation_unary(name)), "{name}");
        }
        assert!(ACTIVE_RPC_METHODS.contains(&read_unary("device.createMediaTicket")));
    }

    #[test]
    fn method_spec_constructors_preserve_name_and_mode_at_runtime() {
        assert_eq!(
            read_unary("runtime.unary"),
            RpcMethodSpec {
                name: "runtime.unary",
                mode: MethodMode::Unary,
                mutability: MethodMutability::Read,
            }
        );
        assert_eq!(
            read_stream("runtime.stream"),
            RpcMethodSpec {
                name: "runtime.stream",
                mode: MethodMode::Stream,
                mutability: MethodMutability::Read,
            }
        );
    }

    #[test]
    fn preview_automation_connect_does_not_hold_a_mutation_permit() {
        assert!(ACTIVE_RPC_METHODS.contains(&read_stream("previewAutomation.connect")));
        assert_eq!(
            method_mutability("previewAutomation.connect"),
            Some(MethodMutability::Read)
        );
    }

    #[test]
    fn passive_vcs_summary_is_a_stream_method() {
        assert!(ACTIVE_RPC_METHODS.contains(&read_stream("subscribeVcsStatusSummary")));
    }

    #[test]
    fn pairing_confirmation_is_an_active_mutation() {
        assert!(ACTIVE_RPC_METHODS.contains(&mutation_unary("auth.confirmPairing")));
    }
}
