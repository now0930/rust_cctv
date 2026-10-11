#!/usr/bin/env bash

set -u

container="${APACHE_CONTAINER:-}"
ssl_config="${APACHE_SSL_CONFIG:-/etc/apache2/sites-enabled/default-ssl.conf}"

if ! command -v docker >/dev/null 2>&1; then
  echo "오류: docker 명령을 찾을 수 없습니다." >&2
  exit 1
fi

if [[ -z "$container" ]]; then
  echo "오류: APACHE_CONTAINER 환경변수로 Apache 컨테이너 이름을 지정하세요." >&2
  exit 1
fi

if ! docker inspect --format '{{.State.Running}}' "$container" 2>/dev/null | grep -qx true; then
  echo "오류: 컨테이너 '$container'가 실행 중이 아닙니다." >&2
  echo "실행 중인 컨테이너 확인: docker ps --format 'table {{.Names}}\t{{.Image}}'" >&2
  exit 1
fi

echo "Apache 진단 대상 컨테이너: $container"
echo "이 스크립트는 설정과 로그를 조회만 하며 변경하지 않습니다."

echo
echo '=== Container mount points ==='
if ! docker inspect --format \
  '{{range .Mounts}}{{printf "%s %s -> %s\n" .Type .Source .Destination}}{{end}}' "$container"; then
  echo "오류: 컨테이너 마운트 정보를 가져오지 못했습니다." >&2
fi

echo
echo '=== Docker networks ==='
if ! docker inspect --format \
  '{{range $name, $network := .NetworkSettings.Networks}}{{println $name}}{{end}}' "$container"; then
  echo "오류: 컨테이너 네트워크 정보를 가져오지 못했습니다." >&2
fi

echo
echo '=== Apache auth modules and htpasswd ==='
if ! docker exec "$container" sh -c \
  'apache2ctl -M 2>/dev/null | grep -E "auth_basic_module|authn_file_module|authz_user_module" || true; if command -v htpasswd >/dev/null 2>&1; then echo "htpasswd: $(command -v htpasswd)"; else echo "htpasswd: unavailable"; fi'; then
  echo "오류: Apache 인증 모듈 정보를 가져오지 못했습니다." >&2
fi

echo
echo '=== Moonfire NVR container status and networks ==='
moonfire_container="${MOONFIRE_CONTAINER:-}"
if [[ -z "$moonfire_container" ]]; then
  moonfire_container="$(docker compose ps -q moonfire-nvr 2>/dev/null || true)"
fi
if docker inspect --format '{{.State.Running}}' "$moonfire_container" 2>/dev/null | grep -qx true; then
  echo "컨테이너: $moonfire_container (running)"
  if ! docker inspect --format \
    '{{range $name, $network := .NetworkSettings.Networks}}{{println $name}}{{end}}' "$moonfire_container"; then
    echo "오류: Moonfire NVR 네트워크 정보를 가져오지 못했습니다." >&2
  fi
else
  echo "Moonfire NVR 컨테이너 '$moonfire_container'를 찾지 못했거나 실행 중이 아닙니다."
fi

echo
echo '=== Apache virtual hosts ==='
if ! docker exec "$container" apache2ctl -S; then
  echo "오류: 가상 호스트 정보를 가져오지 못했습니다." >&2
fi

echo
echo '=== Enabled site configuration ==='
if ! docker exec "$container" sh -c \
  "grep -RniE 'ServerName|ServerAlias|DocumentRoot|ProxyPass|SSLCertificate(File|KeyFile)|<Directory|Require all|ErrorLog' /etc/apache2/sites-enabled/"; then
  echo "오류: 사이트 설정을 읽지 못했거나 검색 결과가 없습니다." >&2
fi

echo
echo '=== SSL virtual host configuration (lines 1-240) ==='
if ! docker exec "$container" sh -c \
  "config='$ssl_config'; if [ -r \"\$config\" ]; then nl -ba \"\$config\" | sed -n '1,240p'; else echo \"오류: \$config 파일을 읽을 수 없습니다.\" >&2; exit 1; fi"; then
  echo "오류: SSL 가상 호스트 설정 일부를 가져오지 못했습니다." >&2
fi

echo
echo '=== Recent Apache error log ==='
if ! docker exec "$container" sh -c \
  'if [ -r /var/log/apache2/error.log ]; then tail -n 80 /var/log/apache2/error.log; else echo "오류: /var/log/apache2/error.log 파일을 읽을 수 없습니다." >&2; exit 1; fi'; then
  echo "오류: Apache 오류 로그를 가져오지 못했습니다." >&2
fi
