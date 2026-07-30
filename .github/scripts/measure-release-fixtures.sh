#!/usr/bin/env bash
set -euo pipefail

epoch_millis() {
  python3 -c 'import time; print(time.time_ns() // 1_000_000)'
}

measure_command() {
  local output_prefix="$1"
  shift
  local started
  started="$(epoch_millis)"
  "$@" &
  local process_id=$!
  local maximum_memory=0
  while kill -0 "$process_id" >/dev/null 2>&1; do
    local current_memory
    current_memory="$(ps -o rss= -p "$process_id" 2>/dev/null | awk '{print $1 * 1024}' || true)"
    if [[ -n "$current_memory" ]] && ((current_memory > maximum_memory)); then
      maximum_memory="$current_memory"
    fi
    sleep 0.05
  done
  wait "$process_id"
  local elapsed
  elapsed="$(($(epoch_millis) - started))"
  printf -v "${output_prefix}_milliseconds" '%s' "$elapsed"
  printf -v "${output_prefix}_memory" '%s' "$maximum_memory"
}

measure_command analysis \
  cargo test -p yt-media-engine --all-features --lib \
  analysis::ytdlp::tests::adaptive_fixture_has_unique_descending_heights_and_merge -- --exact
measure_command download \
  cargo test -p yt-media-engine --all-features --test download_job

{
  echo "YT_MEDIA_ANALYSIS_MS=${analysis_milliseconds}"
  echo "YT_MEDIA_ACTIVE_MEMORY_BYTES=${download_memory}"
} >>"$GITHUB_ENV"
echo "fixture_analysis_ms=${analysis_milliseconds} active_download_memory_bytes=${download_memory} download_fixture_ms=${download_milliseconds}"
