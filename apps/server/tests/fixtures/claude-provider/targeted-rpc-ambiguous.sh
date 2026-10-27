#!/bin/sh
case "$1" in
  --version) printf '%s\n' '2.1.218'; exit 0;;
  --help) printf '%s\n' '--include-hook-events --forward-subagent-text'; exit 0;;
esac

capture='__CAPTURE__'
settings_capture='__SETTINGS__'
token_capture='__TOKEN__'
session_capture='__SESSION_PATH__'
ready_capture='__READY__'
case "$capture" in __*__) capture="$PWD/.coddy-studio-claude-targeted-ambiguous-requests.ndjson";; esac
case "$settings_capture" in __*__) settings_capture="$PWD/.coddy-studio-claude-targeted-ambiguous-settings.json";; esac
case "$token_capture" in __*__) token_capture="$PWD/.coddy-studio-claude-targeted-ambiguous-token";; esac
case "$session_capture" in __*__) session_capture="$PWD/.coddy-studio-claude-targeted-ambiguous-session";; esac
case "$ready_capture" in __*__) ready_capture="$PWD/.coddy-studio-claude-targeted-ambiguous-ready";; esac
has_session=false
for argument in "$@"; do
  case "$argument" in --session-id|--resume) has_session=true;; esac
done
if [ "$has_session" = false ]; then
  capture=/dev/null
  settings_capture=/dev/null
  token_capture=/dev/null
  session_capture=/dev/null
  ready_capture=/dev/null
fi

session_id=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    --session-id|--resume)
      shift
      session_id="$1"
      ;;
    --settings)
      shift
      printf '%s' "$1" > "$settings_capture"
      ;;
  esac
  shift
done
printf '%s' "$CODDY_STUDIO_CLAUDE_HOOK_TOKEN" > "$token_capture"
printf '%s' "$session_id" > "$session_capture"
if [ -s "$settings_capture" ] && [ -s "$token_capture" ] && [ -s "$session_capture" ]; then
  : > "$ready_capture"
fi

emit() {
  printf '%s\n' "$1" | sed "s/__SESSION__/$session_id/g"
}

emit '{"type":"stream_event","session_id":"__SESSION__","uuid":"tool-parent","parent_tool_use_id":null,"event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool-agent-parent","name":"Agent","input":{"description":"parent","prompt":"parent","subagent_type":"same-role"}}}}'
emit '{"type":"system","subtype":"task_started","session_id":"__SESSION__","uuid":"task-parent-start","task_id":"task-parent","tool_use_id":"tool-agent-parent","task_type":"remote_agent","description":"parent"}'

emit '{"type":"stream_event","session_id":"__SESSION__","uuid":"tool-child-one","parent_tool_use_id":"tool-agent-parent","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool-agent-child-one","name":"Agent","input":{"description":"same description","prompt":"same prompt"}}}}'
emit '{"type":"system","subtype":"task_started","session_id":"__SESSION__","uuid":"task-child-one-start","task_id":"task-child-one","tool_use_id":"tool-agent-child-one","task_type":"local_agent"}'

emit '{"type":"stream_event","session_id":"__SESSION__","uuid":"tool-child-two","parent_tool_use_id":"tool-agent-parent","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool-agent-child-two","name":"Agent","input":{"description":"same description","prompt":"same prompt"}}}}'
emit '{"type":"system","subtype":"task_started","session_id":"__SESSION__","uuid":"task-child-two-start","task_id":"task-child-two","tool_use_id":"tool-agent-child-two","task_type":"remote_agent"}'

while IFS= read -r line; do
  printf '%s\n' "$line" >> "$capture"
  case "$line" in
    *'"subtype":"stop_task"'*)
      request_id=$(printf '%s\n' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{}}}\n' "$request_id"
      ;;
  esac
done
