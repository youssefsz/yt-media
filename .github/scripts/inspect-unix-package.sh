#!/usr/bin/env bash
set -euo pipefail

bundle_directory="$1"
stage_directory="$2"
target="$3"
inspection_root="${RUNNER_TEMP}/inspect-${target}"
rm -rf -- "$inspection_root"
mkdir -p -- "$inspection_root"

expected_digest() {
  local qualified_name="$1"
  awk -v name="$qualified_name" '$2 == name { print $1 }' "${stage_directory}/SHA256SUMS"
}

verify_tree() {
  local root="$1"
  for tool in yt-dlp ffmpeg ffprobe deno; do
    local qualified="${tool}-${target}"
    local expected
    expected="$(expected_digest "$qualified")"
    test -n "$expected"
    local matches=()
    while IFS= read -r -d '' match; do
      matches[${#matches[@]}]="$match"
    done < <(find "$root" -type f -name "$tool" -print0)
    test "${#matches[@]}" -eq 1
    local found
    found="$(shasum -a 256 "${matches[0]}" | awk '{print $1}')"
    test "$found" = "$expected"
  done
  if find "$root" \( -name '*.part' -o -name '*.tmp' -o -name '.cache' -o -path '*/staging/*' -o -path '*/updates/*' \) -print -quit | grep -q .; then
    echo "package contains cache, partial, staging, or update state" >&2
    return 1
  fi
}

if [[ "$target" == *apple-darwin ]]; then
  dmg="$(find "$bundle_directory" -type f -name '*.dmg' -print -quit)"
  test -n "$dmg"
  mount_point="${inspection_root}/mount"
  mkdir -p -- "$mount_point"
  hdiutil attach "$dmg" -readonly -nobrowse -mountpoint "$mount_point"
  trap 'hdiutil detach "$mount_point" >/dev/null 2>&1 || true' EXIT
  app="$(find "$mount_point" -type d -name '*.app' -print -quit)"
  test -n "$app"
  verify_tree "${app}/Contents/MacOS"
  codesign --verify --deep --strict "$app"
  hdiutil detach "$mount_point"
  trap - EXIT
  bytes="$(stat -f '%z' "$dmg")"
  digest="$(shasum -a 256 "$dmg" | awk '{print $1}')"
  echo "validated_desktop_artifact=$(basename "$dmg") bytes=${bytes} sha256=${digest}"
else
  deb="$(find "$bundle_directory" -type f -name '*.deb' -print -quit)"
  appimage="$(find "$bundle_directory" -type f -name '*.AppImage' -print -quit)"
  test -n "$deb"
  test -n "$appimage"
  deb_root="${inspection_root}/deb"
  mkdir -p -- "$deb_root"
  dpkg-deb -x "$deb" "$deb_root"
  verify_tree "$deb_root"
  appimage_root="${inspection_root}/appimage"
  mkdir -p -- "$appimage_root"
  (
    cd "$appimage_root"
    "$appimage" --appimage-extract >/dev/null
  )
  verify_tree "${appimage_root}/squashfs-root"
  for artifact in "$deb" "$appimage"; do
    bytes="$(stat -c '%s' "$artifact")"
    digest="$(sha256sum "$artifact" | awk '{print $1}')"
    echo "validated_desktop_artifact=$(basename "$artifact") bytes=${bytes} sha256=${digest}"
  done
fi
