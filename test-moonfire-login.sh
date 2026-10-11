#!/usr/bin/env bash

set -Eeuo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
export CCTV_URL="${CCTV_URL:-}"
export NVR_DIRECT_URL="${NVR_DIRECT_URL:-http://127.0.0.1:8081}"
command -v python3 >/dev/null 2>&1 || {
  echo "오류: python3를 찾을 수 없습니다." >&2
  exit 1
}

read -r -p "Apache Basic 인증 사용자 이름: " apache_user
read -r -s -p "Apache Basic 인증 비밀번호: " apache_password
printf '\n'
read -r -p "Moonfire NVR 사용자 이름: " moonfire_user
read -r -s -p "Moonfire NVR 비밀번호: " moonfire_password
printf '\n'

printf '%s\n' "$apache_user" "$apache_password" "$moonfire_user" "$moonfire_password" |
  python3 "$script_dir/test-moonfire-login.py"
