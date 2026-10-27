#![cfg(windows)]

use std::{sync::Arc, time::Duration};

use coddy_studio_server::terminal::{
    PortablePtyBackend, TerminalAttachInput, TerminalManager, TerminalManagerOptions,
    TerminalOpenInput,
};

const PREDICTION_NONE: &str = "PSPRED=None";
const PREDICTION_UNSUPPORTED: &str = "PSPRED=UNSUPPORTED";
const PREDICTION_QUERY: &str = "$o = Get-PSReadLineOption; if ($o.PSObject.Properties.Name -contains 'PredictionSource') { Write-Output ('PSPRED=' + $o.PredictionSource) } else { Write-Output ('PSPRED=' + ('UN' + 'SUPPORTED')) }\r\n";
const HOST_QUERY: &str = "Write-Output ('PSHOST=' + $PSVersionTable.PSEdition)\r\n";
const TEMPORARY_GLOBALS_QUERY: &str = "$count = @(Get-Variable originalPrompt,cleanupPrompt -Scope Global -ErrorAction SilentlyContinue).Count; Write-Output ('PSTEMP=' + $count)\r\n";

/// Reads the current transcript via a fresh re-attach snapshot.
async fn read_history(manager: &TerminalManager) -> String {
    manager
        .attach(TerminalAttachInput::existing(
            "thread-pred",
            "terminal-pred",
        ))
        .await
        .expect("terminal remains attachable")
        .initial
        .history
}

/// Polls the transcript until `matches` accepts it. Does NOT reply to the cursor query —
/// the reply is sent exactly once by the caller (mirroring terminal_default_shell.rs) so
/// a repeated `\u{1b}[1;1R` cannot leak stray `R` characters into the command line.
async fn wait_for_history(
    manager: &TerminalManager,
    matches: impl Fn(&str) -> bool,
) -> Option<String> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let history = read_history(manager).await;
            if matches(&history) {
                return history;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .ok()
}

#[tokio::test]
async fn managed_pwsh_disables_predictive_history_without_leaking_the_bootstrap() {
    run_managed_powershell_proof("pwsh.exe", "Core").await;
}

#[tokio::test]
async fn managed_windows_powershell_disables_predictive_history_without_leaking_the_bootstrap() {
    run_managed_powershell_proof("powershell.exe", "Desktop").await;
}

async fn run_managed_powershell_proof(shell: &str, expected_edition: &str) {
    let workspace = tempfile::tempdir().expect("workspace");
    let manager = TerminalManager::new(
        Arc::new(PortablePtyBackend),
        TerminalManagerOptions {
            preferred_shell: Some(shell.to_owned()),
            ..TerminalManagerOptions::default()
        },
    );
    let result: Result<(), String> = async {
        if PREDICTION_QUERY.contains(PREDICTION_NONE)
            || PREDICTION_QUERY.contains(PREDICTION_UNSUPPORTED)
        {
            return Err("completion sentinels must be absent from echoed input".to_owned());
        }
        manager
            .open(TerminalOpenInput::new(
                "thread-pred",
                "terminal-pred",
                workspace.path().to_path_buf(),
                120,
                30,
            ))
            .await
            .map_err(|error| format!("terminal opens: {error}"))?;

        // Windows PowerShell may emit a cursor-position query before its prompt,
        // while PowerShell 7 can render the prompt directly. Both mean the
        // managed shell is ready for input.
        let readiness = wait_for_history(&manager, |history| {
            history.contains("\u{1b}[6n")
                || (history.contains("PS ") && history.contains('>'))
        })
        .await;
        let readiness = if let Some(readiness) = readiness {
            readiness
        } else {
            return Err(format!(
                "PowerShell must reach an interactive prompt, transcript: {:?}",
                read_history(&manager).await
            ));
        };
        if readiness.contains("\u{1b}[6n") {
            manager
                .write("thread-pred", "terminal-pred", "\u{1b}[1;1R")
                .await
                .map_err(|error| format!("terminal accepts cursor response: {error}"))?;
        }

        // The requested executable must be the process that handled the PTY bootstrap.
        manager
            .write("thread-pred", "terminal-pred", HOST_QUERY)
            .await
            .map_err(|error| format!("terminal accepts host query: {error}"))?;
        let expected_host = format!("PSHOST={expected_edition}");
        let history = wait_for_history(&manager, |history| history.contains(&expected_host))
            .await
            .ok_or_else(|| format!("{shell} host query produced {expected_host}"))?;
        if !history.contains(&expected_host) {
            return Err(format!(
                "expected {shell} to report {expected_host}, transcript: {history:?}"
            ));
        }

        // Setup ran in a child scope; neither closure-capture name may leak into globals.
        manager
            .write("thread-pred", "terminal-pred", TEMPORARY_GLOBALS_QUERY)
            .await
            .map_err(|error| format!("terminal accepts temporary-global query: {error}"))?;
        let history = wait_for_history(&manager, |history| history.contains("PSTEMP=0"))
            .await
            .ok_or_else(|| "bootstrap temporary globals must not leak".to_owned())?;
        if !history.contains("PSTEMP=0") {
            return Err(format!(
                "bootstrap temporary globals must not leak, transcript: {history:?}"
            ));
        }

        // Query the effective PredictionSource. The unsupported completion marker is assembled
        // at runtime, so both accepted values are output-only and cannot match echoed input.
        manager
            .write("thread-pred", "terminal-pred", PREDICTION_QUERY)
            .await
            .map_err(|error| format!("terminal accepts prediction query: {error}"))?;

        let history = wait_for_history(&manager, |history| {
            history.contains(PREDICTION_NONE) || history.contains(PREDICTION_UNSUPPORTED)
        })
        .await
        .ok_or_else(|| "PredictionSource query produced output".to_owned())?;

        // A version that supports prediction MUST report None; an older version is accepted.
        let banned = ["PSPRED=History", "PSPRED=Plugin", "PSPRED=HistoryAndPlugin"];
        if banned.iter().any(|value| history.contains(value)) {
            return Err(format!(
                "predictive history must be disabled, transcript: {history:?}"
            ));
        }
        if !(history.contains(PREDICTION_NONE) || history.contains(PREDICTION_UNSUPPORTED)) {
            return Err(format!(
                "expected None or UNSUPPORTED, transcript: {history:?}"
            ));
        }

        // The startup bootstrap must not appear in interactive command history.
        manager
            .write(
                "thread-pred",
                "terminal-pred",
                "$needle = 'Set-PSReadLine' + 'Option'; Write-Output \"HIST=$([bool]((Get-History | Out-String) -match $needle))\"\r\n",
            )
            .await
            .map_err(|error| format!("terminal accepts history query: {error}"))?;
        let history = wait_for_history(&manager, |history| {
            history.contains("HIST=False") || history.contains("HIST=True")
        })
        .await
        .ok_or_else(|| "history query produced output".to_owned())?;
        if !history.contains("HIST=False") {
            return Err(format!(
                "the bootstrap must not appear in interactive command history, transcript: {history:?}"
            ));
        }

        Ok(())
    }
    .await;

    manager.shutdown().await;
    result.unwrap_or_else(|error| panic!("managed {shell} regression proof: {error}"));
}
