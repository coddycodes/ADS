# Provider setup

Coddy Studio supports five coding-agent providers, but only Coddy is enabled by
default. Enable another driver in
[`~/.coddy/studio/config.yaml`](../reference/studio-config.md). Every Chat turn
also requires the Coddy Device Agent on the server host.

| Provider                             | Command        | Sign in               |
| ------------------------------------ | -------------- | --------------------- |
| [Coddy](../providers/coddy.md)       | `coddy`        | `coddy login`         |
| [Codex](../providers/codex.md)       | `codex`        | `codex login`         |
| [Claude](../providers/claude.md)     | `claude`       | `claude auth login`   |
| [Cursor](../providers/cursor.md)     | `cursor-agent` | `cursor-agent login`  |
| [OpenCode](../providers/opencode.md) | `opencode`     | `opencode auth login` |

Install and authenticate at least one provider before starting a chat. The
provider executable and its credentials must be available on the machine that
runs the Coddy Studio server:

- For the desktop app, that is normally the desktop machine.
- For a headless, SSH, or WSL environment, install and authenticate the provider
  inside that environment.
- Browser clients do not run provider CLIs locally.

Coddy Studio resolves provider executables from the server process's `PATH`. You can
also set an explicit **Binary path** on a provider instance in Settings.

Provider instances can have their own display name, accent color, configuration,
and environment variables. Mark API keys and tokens as **Sensitive** so Coddy Studio
stores them separately as server secrets and returns only a redacted value to the
app after saving.

See the [provider guides](../providers/README.md) for provider-specific behavior
and limitations.
