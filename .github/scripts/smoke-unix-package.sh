#!/usr/bin/env bash
set -euo pipefail

epoch_millis() {
  python3 -c 'import time; print(time.time_ns() // 1_000_000)'
}

bundle_directory="$1"
target="$2"
smoke_root="${RUNNER_TEMP}/smoke-${target}"
home_directory="${smoke_root}/home"
rm -rf -- "$smoke_root"
mkdir -p -- "$home_directory"

export HOME="$home_directory"
export XDG_DATA_HOME="${home_directory}/.local/share"
export HTTP_PROXY='http://127.0.0.1:9'
export HTTPS_PROXY='http://127.0.0.1:9'
export ALL_PROXY='http://127.0.0.1:9'
export NO_PROXY=''

cleanup_process() {
  if [[ -n "${application_pid:-}" ]]; then
    kill "$application_pid" >/dev/null 2>&1 || true
    wait "$application_pid" >/dev/null 2>&1 || true
  fi
  if [[ -n "${mount_point:-}" ]] && mount | grep -Fq " on ${mount_point} "; then
    hdiutil detach "$mount_point" >/dev/null 2>&1 || true
  fi
}
trap cleanup_process EXIT

if [[ "$target" == *apple-darwin ]]; then
  artifact="$(find "$bundle_directory" -type f -name '*.dmg' -print -quit)"
  test -n "$artifact"
  mount_point="${smoke_root}/mount"
  mkdir -p -- "$mount_point"
  hdiutil attach "$artifact" -readonly -nobrowse -mountpoint "$mount_point" >/dev/null
  app_bundle="$(find "$mount_point" -type d -name '*.app' -print -quit)"
  test -n "$app_bundle"
  application="$(
    find "${app_bundle}/Contents/MacOS" -type f -perm -111 \
      ! -name 'yt-dlp' ! -name 'ffmpeg' ! -name 'ffprobe' ! -name 'deno' \
      -print -quit
  )"
  test -n "$application"
  installed_bytes="$(du -sk "$app_bundle" | awk '{print $1 * 1024}')"
  "$application" >"${smoke_root}/stdout.log" 2>"${smoke_root}/stderr.log" &
  application_pid=$!
else
  source_artifact="$(find "$bundle_directory" -type f -name '*.AppImage' -print -quit)"
  test -n "$source_artifact"
  artifact="${smoke_root}/$(basename "$source_artifact")"
  cp "$source_artifact" "$artifact"
  chmod 755 "$artifact"
  installed_bytes="$(stat -c '%s' "$artifact")"
  APPIMAGE_EXTRACT_AND_RUN=1 xvfb-run -a "$artifact" >"${smoke_root}/stdout.log" 2>"${smoke_root}/stderr.log" &
  application_pid=$!
fi

start_millis="$(epoch_millis)"
database=''
for _attempt in $(seq 1 30); do
  sleep 0.5
  database="$(find "$home_directory" -type f -name 'jobs.sqlite3' -print -quit)"
  if [[ -n "$database" ]]; then
    break
  fi
  if ! kill -0 "$application_pid" >/dev/null 2>&1; then
    cat "${smoke_root}/stderr.log" >&2
    echo 'packaged application exited during offline startup' >&2
    exit 1
  fi
done
test -n "$database"
cold_start_ms="$(($(epoch_millis) - start_millis))"
idle_memory_bytes="$(ps -o rss= -p "$application_pid" | awk '{print $1 * 1024}')"
kill "$application_pid"
wait "$application_pid" >/dev/null 2>&1 || true
application_pid=''

if [[ "$target" == *apple-darwin ]]; then
  hdiutil detach "$mount_point" >/dev/null
  mount_point=''
else
  rm -f -- "$artifact"
fi
test -f "$database"

{
  echo "YT_MEDIA_INSTALLED_BYTES=${installed_bytes}"
  echo "YT_MEDIA_COLD_START_MS=${cold_start_ms}"
  echo "YT_MEDIA_IDLE_MEMORY_BYTES=${idle_memory_bytes}"
} >>"$GITHUB_ENV"
echo "offline_startup_ms=${cold_start_ms} idle_memory_bytes=${idle_memory_bytes} installed_bytes=${installed_bytes} persistence=preserved uninstall=passed"
