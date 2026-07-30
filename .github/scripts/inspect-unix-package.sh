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
  local mode="${2:-raw}"
  for tool in yt-dlp ffmpeg ffprobe deno; do
    local qualified="${tool}-${target}"
    local expected
    expected="$(expected_digest "$qualified")"
    if [[ -z "$expected" ]]; then
      echo "desktop checksum inventory omitted ${qualified}" >&2
      return 1
    fi
    local matches=()
    while IFS= read -r -d '' match; do
      matches[${#matches[@]}]="$match"
    done < <(find "$root" -type f -name "$tool" -print0)
    if [[ "${#matches[@]}" -ne 1 ]]; then
      echo "expected one packaged ${tool} below ${root}; found ${#matches[@]}" >&2
      return 1
    fi
    local digest_path="${matches[0]}"
    if [[ "$mode" == "signed-macos" ]]; then
      codesign --verify --strict "$digest_path"
      digest_path="${inspection_root}/unsigned-${tool}"
      cp -- "${matches[0]}" "$digest_path"
      codesign --remove-signature "$digest_path"
    fi
    local found
    found="$(shasum -a 256 "$digest_path" | awk '{print $1}')"
    if [[ "$found" != "$expected" ]]; then
      echo "packaged ${tool} payload digest ${found} differs from ${expected}" >&2
      return 1
    fi
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
  verify_tree "${app}/Contents/MacOS" signed-macos
  codesign --verify --deep --strict "$app"
  hdiutil detach "$mount_point"
  trap - EXIT
  bytes="$(stat -f '%z' "$dmg")"
  digest="$(shasum -a 256 "$dmg" | awk '{print $1}')"
  echo "validated_desktop_artifact=$(basename "$dmg") bytes=${bytes} sha256=${digest}"
else
  deb="$(find "$bundle_directory" -type f -name '*.deb' -print -quit)"
  appimage="$(find "$bundle_directory" -type f -name '*.AppImage' -print -quit)"
  if [[ -z "$deb" || -z "$appimage" ]]; then
    echo "expected both Debian and AppImage packages below ${bundle_directory}" >&2
    exit 1
  fi
  appimage="$(realpath "$appimage")"
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
