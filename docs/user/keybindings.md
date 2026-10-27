# Keybindings

Coddy Studio reads production keybindings from:

- `~/.coddy/studio/userdata/keybindings.json`

When `--base-dir` or `CODDY_STUDIO_HOME` changes the server base directory, the path
is `<base-dir>/userdata/keybindings.json`. A server launched with a development
URL uses `<base-dir>/dev/keybindings.json` instead. The safest way to find the
active file is **Settings → Keybindings → Open keybindings.json**, which uses the
path reported by the connected server.

The file must be a JSON array of rules:

```json
[
  { "key": "mod+g", "command": "terminal.newCenter" },
  { "key": "mod+shift+g", "command": "terminal.new", "when": "terminalFocus" }
]
```

See the full schema for more details: [`packages/contracts/src/keybindings.ts`](../../packages/contracts/src/keybindings.ts)

## Defaults

The fixed defaults are:

```json
[
  { "key": "mod+b", "command": "sidebar.toggle" },
  { "key": "mod+j", "command": "terminal.newCenter" },
  { "key": "mod+alt+b", "command": "rightPanel.toggle" },
  { "key": "mod+d", "command": "terminal.split", "when": "terminalFocus" },
  { "key": "mod+shift+d", "command": "terminal.splitVertical", "when": "terminalFocus" },
  { "key": "mod+n", "command": "terminal.new", "when": "terminalFocus" },
  { "key": "mod+w", "command": "terminal.close", "when": "terminalFocus" },
  { "key": "mod+d", "command": "diff.toggle", "when": "!terminalFocus" },
  { "key": "mod+shift+j", "command": "preview.toggle" },
  { "key": "mod+r", "command": "preview.refresh", "when": "previewFocus" },
  { "key": "mod+l", "command": "preview.focusUrl", "when": "previewFocus" },
  { "key": "mod+=", "command": "preview.zoomIn", "when": "previewFocus" },
  { "key": "mod++", "command": "preview.zoomIn", "when": "previewFocus" },
  { "key": "mod+-", "command": "preview.zoomOut", "when": "previewFocus" },
  { "key": "mod+0", "command": "preview.resetZoom", "when": "previewFocus" },
  { "key": "mod+k", "command": "commandPalette.toggle", "when": "!terminalFocus" },
  { "key": "mod+n", "command": "chat.new", "when": "!terminalFocus" },
  { "key": "mod+shift+o", "command": "chat.new", "when": "!terminalFocus" },
  { "key": "mod+shift+n", "command": "chat.newLocal", "when": "!terminalFocus" },
  { "key": "mod+shift+m", "command": "modelPicker.toggle", "when": "!terminalFocus" },
  { "key": "mod+shift+enter", "command": "thread.steerQueuedMessage", "when": "editableFocus" },
  { "key": "mod+o", "command": "editor.openFavorite" },
  { "key": "mod+shift+[", "command": "thread.previous" },
  { "key": "mod+shift+]", "command": "thread.next" }
]
```

Coddy Studio also generates these numbered defaults:

- `mod+1` through `mod+9` run `thread.jump.1` through `thread.jump.9`.
- While the model picker is open, the same keys run `modelPicker.jump.1` through
  `modelPicker.jump.9`.

The native server validates overrides in
[`apps/server/src/production/keybindings.rs`](../../apps/server/src/production/keybindings.rs)
and exposes the resolved file path and configuration issues through
[`apps/server/src/production/control.rs`](../../apps/server/src/production/control.rs).

## Configuration

### Rule Shape

Each entry supports:

- `key` (required): shortcut string, like `mod+j`, `ctrl+k`, `cmd+shift+d`
- `command` (required): action ID
- `when` (optional): boolean expression controlling when the shortcut is active

Invalid rules are ignored. A malformed config file contributes no overrides.
The server reports both cases as configuration issues so the connected app can
surface them.

### Available Commands

- `sidebar.toggle`: open/close the left workspace panel
- `terminal.newCenter`: create and focus a terminal tab in the focused center pane
- `terminal.split`: create a right-hand center split when a center terminal is focused, or split within the right-panel terminal when it owns focus
- `terminal.splitVertical`: create a lower center split when a center terminal is focused, or split vertically within the right-panel terminal when it owns focus
- `terminal.new`: create a terminal tab when a center terminal is focused, or create a terminal in the focused right-panel terminal group
- `terminal.close`: close/kill the focused center or right-panel terminal
- `rightPanel.toggle`: open/close the right tool panel
- `diff.toggle`: open/close the thread diff view
- `preview.toggle`: open/close the in-app browser preview when the active host exposes preview support
- `preview.refresh`: reload the active preview tab (in focused preview context by default)
- `preview.focusUrl`: focus the URL input of the preview panel (in focused preview context by default)
- `preview.zoomIn`: zoom the preview viewport in one step (in focused preview context by default)
- `preview.zoomOut`: zoom the preview viewport out one step (in focused preview context by default)
- `preview.resetZoom`: reset the preview zoom to 100% (in focused preview context by default)
- `commandPalette.toggle`: open or close the global command palette
- `chat.new`: create a new worktree/chat thread preserving the active context where possible
- `chat.newLocal`: create a new chat thread for the active project in a new environment (local/worktree determined by app settings (default `local`))
- `modelPicker.toggle`: open/close the model picker
- `editor.openFavorite`: open current project/worktree in the last-used editor
- `thread.previous` / `thread.next`: jump through visible left-panel workspace cards
- `thread.steerQueuedMessage`: steer the first queued message into the running
  turn on Codex or Claude; defaults to `mod+shift+enter` with
  `when: "editableFocus"`. The composer consumes this shortcut without inserting
  a newline; unavailable steering leaves the draft unchanged.
- `thread.jump.1` through `thread.jump.9`: jump to a visible left-panel workspace card
- `modelPicker.jump.1` through `modelPicker.jump.9`: jump to a model/provider row while the model picker is open
- `script.{id}.run`: run a project script by id (for example `script.test.run`)

Git Manager currently registers no `KeybindingCommand` values. Open it from the
project header's **Git Manager** button, then use the panel's keyboard-operable
lists, menus, dialogs, and buttons. RPC tags such as `gitManager.getRefs` and
`gitManager.runOperation` are protocol method names, not keybinding commands;
putting them in `keybindings.json` makes that rule invalid and it is ignored.

Pull Requests registers no `KeybindingCommand` values. Open it with the
project-header **Pull Requests** button and use its keyboard-operable tabs,
menus, forms, and dialogs. `pullRequests.*` names are RPC methods, not
keybinding commands. Comment editors support Ctrl/Cmd+Enter locally.

### Key Syntax

Supported modifiers:

- `mod` (`cmd` on macOS, `ctrl` on non-macOS)
- `cmd` / `meta`
- `ctrl` / `control`
- `shift`
- `alt` / `option`

Examples:

- `mod+j`
- `mod+shift+d`
- `ctrl+l`
- `cmd+k`

### `when` Conditions

Currently available context keys:

- `terminalFocus`
- `terminalOpen`
- `previewFocus`
- `previewOpen`
- `modelPickerOpen`
- `editableFocus`

Supported operators:

- `!` (not)
- `&&` (and)
- `||` (or)
- parentheses: `(` `)`

Examples:

- `"when": "terminalFocus"`
- `"when": "terminalOpen && !terminalFocus"`
- `"when": "terminalFocus || terminalOpen"`

Unknown condition keys evaluate to `false`.

`terminalFocus` is true when either a center or right-panel terminal owns focus.
`terminalOpen` is true when the active thread has at least one terminal surface
in the center or right panel. The retired bottom terminal drawer is not part of
either context.

### Precedence

- Rules are evaluated in array order.
- For a key event, the last rule where both `key` matches and `when` evaluates to `true` wins.
- That means precedence is across commands, not only within the same command.
