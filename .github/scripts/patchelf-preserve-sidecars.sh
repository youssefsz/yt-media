#!/usr/bin/env bash
set -euo pipefail

real_patchelf="${YT_MEDIA_REAL_PATCHELF:?YT_MEDIA_REAL_PATCHELF must identify the trusted system patchelf}"

if [[ "$#" -eq 3 && "$1" == "--set-rpath" ]]; then
  case "$(basename "$3")" in
    yt-dlp | ffmpeg | ffprobe | deno)
      echo "preserving authenticated sidecar without RPATH mutation: $3" >&2
      exit 0
      ;;
  esac
fi

exec "$real_patchelf" "$@"
