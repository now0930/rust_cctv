#!/usr/bin/env bash

set -Eeuo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$script_dir"

command -v docker >/dev/null 2>&1 || {
  echo "오류: docker 명령을 찾을 수 없습니다." >&2
  exit 1
}

if ! docker compose version >/dev/null 2>&1; then
  echo "오류: Docker Compose 플러그인을 실행할 수 없습니다." >&2
  exit 1
fi

stopped=0
debug_log=""
cleanup() {
  status=$?
  trap - EXIT
  if [[ -n "$debug_log" ]]; then
    rm -f -- "$debug_log"
  fi
  if (( stopped )); then
    echo
    echo "Moonfire NVR 서비스를 다시 시작합니다."
    if ! PUID="$(id -u)" PGID="$(id -g)" docker compose up -d moonfire-nvr; then
      echo "오류: Moonfire NVR을 다시 시작하지 못했습니다. 'docker compose up -d moonfire-nvr'로 확인하세요." >&2
      exit 1
    fi
  fi
  exit "$status"
}
trap cleanup EXIT

echo "Moonfire NVR을 잠시 중지합니다. 진행 중인 녹화가 중단될 수 있습니다."
PUID="$(id -u)" PGID="$(id -g)" docker compose stop moonfire-nvr
stopped=1

echo
debug_log="$(mktemp "${TMPDIR:-/tmp}/moonfire-config.XXXXXX")"

echo "Moonfire 설정 화면에서 Users 항목을 선택해 사용자를 추가하거나 비밀번호를 재설정하세요."
echo "저장 후 설정 화면을 종료하면 서비스가 자동으로 다시 시작됩니다."
echo "설정 로그는 일시적으로 $debug_log 에 기록됩니다."
if ! PUID="$(id -u)" PGID="$(id -g)" docker compose run --rm moonfire-nvr config 2>"$debug_log"; then
  echo "오류: Moonfire 설정 도구가 비정상 종료했습니다. 진단 로그:" >&2
  cat "$debug_log" >&2
  exit 1
fi

if [[ -s "$debug_log" ]]; then
  echo "Moonfire 설정 로그:" >&2
  cat "$debug_log" >&2
fi
