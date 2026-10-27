# Studio config (`config.yaml`)

Coddy Studio reads an optional operator file at:

```text
$CODDY_STUDIO_HOME/config.yaml
# usually:
~/.coddy/studio/config.yaml
```

It is the product policy for which agent drivers and model providers Studio may
use. It is read on every settings load; a malformed file is reported on stderr
and the Coddy-only defaults are used instead.

## Defaults

With no file:

- The **Coddy** driver is enabled. Codex, Claude, Cursor, Grok and OpenCode are
  off.
- The model catalog offers only models whose provider is `coddy` — the Coddy
  API at `https://api.coddy.id/v1`, discovered from `/v1/models`.
- The **Coddy Device Agent** is required; Coddy turns are refused while
  `http://127.0.0.1:9098/api/v1/agent/health` does not answer.

## Enabling another driver

```yaml
providers:
  codex:
    enabled: true
  claudeAgent:
    enabled: true
```

Only drivers named here change. A driver can also be explicitly disabled:

```yaml
providers:
  opencode:
    enabled: false
```

Driver keys: `coddy`, `codex`, `claudeAgent`, `cursor`, `grok`, `opencode`.
Setting `enabled: true` does not install the CLI; the provider still needs its
binary on the server and its own login.

## Widening the model catalog

```yaml
models:
  providers:
    - coddy
    - openai
    - anthropic
```

The list holds model-provider IDs. When it is absent or empty, only `coddy` is
offered. A driver can be enabled while its models stay hidden; enable both when
you want to use it end to end.

## Device agent

```yaml
deviceAgent:
  required: true
  url: http://127.0.0.1:9098
```

The device agent is a hard prerequisite for Coddy Studio's own agent: it owns
the project registry, the local services and the paired-device tunnel. Keep
`required: true` unless you are developing Studio against a fixture agent.

Set `required: false` only for an isolated development machine; Studio then
skips the health gate and Coddy turns may fail later at the CLI.

## Precedence

- `config.yaml` wins over `settings.json` for every driver it names.
- Drivers it does not name keep the value in `settings.json`; fresh installs
  start with only Coddy enabled.
- The model allowlist always applies to the catalog Studio builds from the
  Coddy CLI (`coddy cli models --json`).
