#!/usr/bin/env bash

set -u

domain="${CCTV_DOMAIN:-}"
webroot="${WEBROOT:-/var/www/html}"
certbot_webroot="${CERTBOT_WEBROOT:-/webroot}"
letsencrypt_dir="${LETSENCRYPT_DIR:-/etc/letsencrypt}"
letsencrypt_work_dir="${LETSENCRYPT_WORK_DIR:-/var/lib/letsencrypt}"

if ! command -v docker >/dev/null 2>&1; then
  echo "오류: docker 명령을 찾을 수 없습니다." >&2
  exit 1
fi

if [ -z "$domain" ]; then
  echo "오류: CCTV_DOMAIN 환경변수로 인증서를 발급할 도메인을 지정하세요." >&2
  exit 1
fi

for dir in "$letsencrypt_dir" "$letsencrypt_work_dir" "$webroot"; do
  if [ ! -d "$dir" ]; then
    echo "오류: 호스트 디렉터리 '$dir'가 없습니다. 변경은 수행하지 않았습니다." >&2
    exit 1
  fi
done

echo "Let's Encrypt에서 $domain 인증서를 발급합니다."
echo "검증 방식: HTTP-01 (포트 80, 웹루트 $webroot)"
echo "인증서 저장 위치: $letsencrypt_dir/live/$domain/"
echo "기존 인증서와 Apache 설정은 변경하지 않습니다."
echo "Certbot이 이메일과 약관 동의를 물으면 안내에 따라 입력하세요."
echo

docker run --rm -it \
  --name cctv-certbot-issue \
  -v "$letsencrypt_dir:/etc/letsencrypt" \
  -v "$letsencrypt_work_dir:/var/lib/letsencrypt" \
  -v "$webroot:$certbot_webroot" \
  certbot/certbot certonly \
  --webroot \
  --webroot-path "$certbot_webroot" \
  --cert-name "$domain" \
  -d "$domain"
