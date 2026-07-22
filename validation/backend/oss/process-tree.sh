#!/usr/bin/env bash

reproit_start_process_group() {
  local working_directory="$1" log_path="$2"
  shift 2
  (
    cd "$working_directory"
    exec perl -MPOSIX -e 'POSIX::setsid() >= 0 or die "setsid: $!"; exec @ARGV' \
      -- "$@" >"$log_path" 2>&1
  ) &
  REPROIT_STARTED_PID=$!
}

reproit_process_group_alive() {
  kill -0 -- "-$1" 2>/dev/null
}

reproit_stop_process_group() {
  local group_id="$1" attempt
  if ! [[ "$group_id" =~ ^[0-9]+$ ]] || ((group_id <= 1)); then
    echo "refusing invalid process-group ID: $group_id" >&2
    return 1
  fi
  kill -TERM -- "-$group_id" 2>/dev/null || true
  for ((attempt = 0; attempt < 30; attempt += 1)); do
    if ! reproit_process_group_alive "$group_id"; then break; fi
    sleep 0.1
  done
  if reproit_process_group_alive "$group_id"; then
    kill -KILL -- "-$group_id" 2>/dev/null || true
  fi
  for ((attempt = 0; attempt < 10; attempt += 1)); do
    if ! reproit_process_group_alive "$group_id"; then break; fi
    sleep 0.1
  done
  if reproit_process_group_alive "$group_id"; then
    echo "process group $group_id survived SIGKILL" >&2
    return 1
  fi
  wait "$group_id" 2>/dev/null || true
}
