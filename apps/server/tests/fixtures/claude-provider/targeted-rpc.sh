#!/bin/sh
case "$1" in
  --version) printf '%s\n' '2.1.231'; exit 0;;
  --help) printf '%s\n' '--include-hook-events --forward-subagent-text'; exit 0;;
esac

capture='__CAPTURE__'
settings_capture='__SETTINGS__'
token_capture='__TOKEN__'
session_capture='__SESSION_PATH__'
ready_capture='__READY__'
case "$capture" in __*__) capture="$PWD/.coddy-studio-claude-targeted-requests.ndjson";; esac
case "$settings_capture" in __*__) settings_capture="$PWD/.coddy-studio-claude-targeted-settings.json";; esac
case "$token_capture" in __*__) token_capture="$PWD/.coddy-studio-claude-targeted-token";; esac
case "$session_capture" in __*__) session_capture="$PWD/.coddy-studio-claude-targeted-session";; esac
case "$ready_capture" in __*__) ready_capture="$PWD/.coddy-studio-claude-targeted-ready";; esac
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

emit '{"type":"assistant","session_id":"__SESSION__","uuid":"tool-a","parent_tool_use_id":null,"message":{"id":"message-a","content":[{"type":"tool_use","id":"tool-agent-a","name":"Agent","input":{"description":"same description","prompt":"same prompt","subagent_type":"same-role"}}]}}'
emit '{"type":"system","subtype":"task_started","session_id":"__SESSION__","uuid":"task-a-start","task_id":"task-a","tool_use_id":"tool-agent-a","task_type":"remote_agent","description":"same description"}'

emit '{"type":"assistant","session_id":"__SESSION__","uuid":"tool-b","parent_tool_use_id":null,"message":{"id":"message-b","content":[{"type":"tool_use","id":"tool-agent-b","name":"Agent","input":{"description":"same description","prompt":"same prompt","subagent_type":"same-role"}}]}}'
emit '{"type":"system","subtype":"task_started","session_id":"__SESSION__","uuid":"task-b-start","task_id":"task-b","tool_use_id":"tool-agent-b","task_type":"local_agent","description":"same description"}'

emit '{"type":"assistant","session_id":"__SESSION__","uuid":"tool-child","parent_tool_use_id":"tool-agent-a","message":{"id":"message-child","content":[{"type":"tool_use","id":"tool-agent-child","name":"Agent","input":{"description":"same description","prompt":"same prompt"}}]}}'
emit '{"type":"system","subtype":"task_started","session_id":"__SESSION__","uuid":"task-child-start","task_id":"task-child","tool_use_id":"tool-agent-child"}'

stop_count=0
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$capture"
  case "$line" in
    *'"subtype":"stop_task"'*)
      request_id=$(printf '%s\n' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
      task_id=$(printf '%s\n' "$line" | sed -n 's/.*"task_id":"\([^"]*\)".*/\1/p')
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{}}}\n' "$request_id"
      if [ "$stop_count" -eq 0 ] && [ "$task_id" = 'task-a' ]; then
        stop_count=1
      elif [ "$stop_count" -eq 1 ] && [ "$task_id" = 'task-child' ]; then
        stop_count=2
        emit '{"type":"system","subtype":"task_notification","session_id":"__SESSION__","uuid":"task-child-stopped","task_id":"task-child","status":"stopped"}'
        emit '{"type":"system","subtype":"task_notification","session_id":"__SESSION__","uuid":"task-a-stopped","task_id":"task-a","status":"stopped"}'
        emit '{"type":"stream_event","session_id":"__SESSION__","uuid":"root-after-cancel","parent_tool_use_id":null,"event":{"type":"content_block_delta","index":9,"delta":{"type":"text_delta","text":"root-after-cancel"}}}'
      fi
      ;;
  esac
done
