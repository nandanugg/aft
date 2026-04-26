#!/usr/bin/env bash
set -euo pipefail

AFT_BINARY="__AFT_BINARY_PATH__"
DEFAULT_GO_OVERLAY_PROVIDER="${AFT_GO_OVERLAY_PROVIDER:-${AFT_GO_OVERLAY_BACKEND:-aft_go_sidecar}}"
DEFAULT_LEASE_TTL_SECS="${AFT_SESSION_LEASE_TTL_SECS:-1800}"
DEFAULT_WARM_TIMEOUT_SECS="${AFT_SESSION_WARM_TIMEOUT_SECS:-300}"

aft_hook_json_value() {
  local json="$1"
  local expr="$2"
  jq -r "$expr // empty" <<<"$json" 2>/dev/null
}

aft_detect_go_root() {
  local start="${1:-$PWD}"
  local dir
  if [ -d "$start" ]; then
    dir="$start"
  else
    dir="$(dirname "$start")"
  fi
  dir="$(cd "$dir" 2>/dev/null && pwd -P)" || return 1

  while [ -n "$dir" ] && [ "$dir" != "/" ]; do
    if [ -f "$dir/go.work" ] || [ -f "$dir/go.mod" ]; then
      printf '%s\n' "$dir"
      return 0
    fi
    dir="$(dirname "$dir")"
  done
  return 1
}

aft_send_session_command() {
  local payload="$1"
  if [ "${AFT_SESSION_DEBUG:-0}" = "1" ]; then
    printf '%s\n' "$payload" | "$AFT_BINARY"
  else
    printf '%s\n' "$payload" | "$AFT_BINARY" 2>/dev/null
  fi
}

aft_session_open() {
  local hook_json="$1"
  local client="$2"
  command -v jq >/dev/null 2>&1 || return 0
  [ -x "$AFT_BINARY" ] || return 0

  local session_id cwd root payload
  session_id="$(aft_hook_json_value "$hook_json" '.session_id')"
  cwd="$(aft_hook_json_value "$hook_json" '.cwd')"
  [ -n "$session_id" ] || return 0
  [ -n "$cwd" ] || cwd="$PWD"
  root="$(aft_detect_go_root "$cwd")" || return 0

  payload="$(
    jq -cn \
      --arg id "hook-session-open" \
      --arg sid "$session_id" \
      --arg root "$root" \
      --arg provider "$DEFAULT_GO_OVERLAY_PROVIDER" \
      --arg client "$client" \
      --argjson ttl "$DEFAULT_LEASE_TTL_SECS" \
      --argjson warm "$DEFAULT_WARM_TIMEOUT_SECS" \
      '{
        id: $id,
        command: "go_overlay_session_open",
        session_id: $sid,
        project_root: $root,
        go_overlay_provider: $provider,
        client: $client,
        lease_ttl_secs: $ttl,
        warm_timeout_secs: $warm
      }'
  )"
  aft_send_session_command "$payload" >/dev/null || true
}

aft_session_touch() {
  local hook_json="$1"
  local client="$2"
  command -v jq >/dev/null 2>&1 || return 0
  [ -x "$AFT_BINARY" ] || return 0

  local session_id cwd root payload
  session_id="$(aft_hook_json_value "$hook_json" '.session_id')"
  cwd="$(aft_hook_json_value "$hook_json" '.cwd')"
  [ -n "$session_id" ] || return 0
  [ -n "$cwd" ] || cwd="$PWD"
  root="$(aft_detect_go_root "$cwd")" || return 0

  payload="$(
    jq -cn \
      --arg id "hook-session-touch" \
      --arg sid "$session_id" \
      --arg root "$root" \
      --arg provider "$DEFAULT_GO_OVERLAY_PROVIDER" \
      --arg client "$client" \
      --argjson ttl "$DEFAULT_LEASE_TTL_SECS" \
      '{
        id: $id,
        command: "go_overlay_session_touch",
        session_id: $sid,
        project_root: $root,
        go_overlay_provider: $provider,
        client: $client,
        lease_ttl_secs: $ttl
      }'
  )"
  aft_send_session_command "$payload" >/dev/null || true
}

aft_session_close() {
  local hook_json="$1"
  local client="$2"
  command -v jq >/dev/null 2>&1 || return 0
  [ -x "$AFT_BINARY" ] || return 0

  local session_id cwd root payload
  session_id="$(aft_hook_json_value "$hook_json" '.session_id')"
  cwd="$(aft_hook_json_value "$hook_json" '.cwd')"
  [ -n "$session_id" ] || return 0
  [ -n "$cwd" ] || cwd="$PWD"
  root="$(aft_detect_go_root "$cwd")" || return 0

  payload="$(
    jq -cn \
      --arg id "hook-session-close" \
      --arg sid "$session_id" \
      --arg root "$root" \
      --arg provider "$DEFAULT_GO_OVERLAY_PROVIDER" \
      --arg client "$client" \
      '{
        id: $id,
        command: "go_overlay_session_close",
        session_id: $sid,
        project_root: $root,
        go_overlay_provider: $provider,
        client: $client
      }'
  )"
  aft_send_session_command "$payload" >/dev/null || true
}
