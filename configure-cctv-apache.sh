#!/usr/bin/env bash

set -Eeuo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$script_dir"

apache_container="${APACHE_CONTAINER:-}"
moonfire_container="${MOONFIRE_CONTAINER:-}"
moonfire_network="${MOONFIRE_NETWORK:-}"
host_config="${APACHE_HOST_CONFIG:-}"
config_marker="# BEGIN rust_cctv CCTV HTTPS proxy"
auth_file="${APACHE_AUTH_FILE:-/tmp/cctv.htpasswd}"
domain="${CCTV_DOMAIN:-}"
temporary_config=""

cleanup() {
  if [[ -n "$temporary_config" ]]; then
    rm -f -- "$temporary_config"
  fi
}
trap cleanup EXIT

fail() {
  echo "오류: $*" >&2
  exit 1
}

command -v docker >/dev/null 2>&1 || fail "docker 명령을 찾을 수 없습니다."
if [[ -z "$moonfire_container" ]]; then
  moonfire_container="$(docker compose ps -q moonfire-nvr 2>/dev/null || true)"
fi
[[ -n "$apache_container" ]] || fail "APACHE_CONTAINER를 지정하세요."
[[ -n "$moonfire_container" ]] || fail "실행 중인 Moonfire 컨테이너를 찾을 수 없습니다. MOONFIRE_CONTAINER를 지정하세요."
[[ -n "$moonfire_network" ]] || fail "MOONFIRE_NETWORK를 지정하세요."
[[ -n "$host_config" ]] || fail "APACHE_HOST_CONFIG를 지정하세요."
[[ -n "$domain" ]] || fail "CCTV_DOMAIN을 지정하세요."
[[ -f "$host_config" && -w "$host_config" ]] ||
  fail "Apache 설정 파일 '$host_config'가 없거나 수정할 수 없습니다."

for container in "$apache_container" "$moonfire_container"; do
  running="$(docker inspect --format '{{.State.Running}}' "$container" 2>/dev/null || true)"
  [[ "$running" == true ]] || fail "컨테이너 '$container'가 실행 중이 아닙니다."
done

for container in "$apache_container" "$moonfire_container"; do
  if ! docker inspect --format \
    '{{range $name, $network := .NetworkSettings.Networks}}{{println $name}}{{end}}' \
    "$container" | grep -Fxq "$moonfire_network"; then
    fail "'$container'가 공용 네트워크 '$moonfire_network'에 연결되어 있지 않습니다."
  fi
done

moonfire_ip="$(docker inspect --format \
  "{{with index .NetworkSettings.Networks \"$moonfire_network\"}}{{.IPAddress}}{{end}}" \
  "$moonfire_container" 2>/dev/null || true)"
[[ -n "$moonfire_ip" ]] ||
  fail "Moonfire 컨테이너의 '$moonfire_network' 네트워크 IP를 확인하지 못했습니다."

if ! docker exec "$apache_container" getent hosts moonfire-nvr >/dev/null 2>&1 &&
   ! docker exec "$apache_container" getent hosts "$moonfire_container" >/dev/null 2>&1; then
  echo "경고: Apache에서 Docker 서비스 이름을 해석하지 못합니다. 확인한 Moonfire IP $moonfire_ip 를 프록시에 사용합니다." >&2
fi

docker exec "$apache_container" test -r \
  "/etc/letsencrypt/live/$domain/fullchain.pem" ||
  fail "Apache 컨테이너에서 발급된 인증서를 읽을 수 없습니다."
docker exec "$apache_container" test -r \
  "/etc/letsencrypt/live/$domain/privkey.pem" ||
  fail "Apache 컨테이너에서 인증서 개인 키를 읽을 수 없습니다."

docker exec -u 0 "$apache_container" sh -c \
  'test -d /tmp/wp_update && test -w /tmp/wp_update' ||
  fail "Apache 컨테이너의 쓰기 가능한 /tmp/wp_update 볼륨을 찾을 수 없습니다. 인증 파일을 저장할 영구 쓰기 가능 위치를 확인하세요."

if docker exec "$apache_container" test -e "$auth_file"; then
  echo "기존 Basic 인증 파일을 사용합니다. 비밀번호 파일은 덮어쓰지 않습니다."
else
  echo "CCTV용 Basic 인증 사용자 이름을 입력하세요."
  read -r -p "사용자 이름: " auth_user
  [[ -n "$auth_user" ]] || fail "사용자 이름은 비워 둘 수 없습니다."
  echo "컨테이너 root 권한으로 인증 파일을 생성합니다: $auth_file"
  if ! docker exec -u 0 -it "$apache_container" htpasswd -cB "$auth_file" "$auth_user"; then
    echo "컨테이너의 쓰기 권한 진단:" >&2
    docker exec -u 0 "$apache_container" sh -c \
      'id; ls -ld /tmp/wp_update; test -w /tmp/wp_update && echo "directory writable" || echo "directory not writable"; grep " /tmp/wp_update " /proc/mounts || true' >&2 || true
    fail "Basic 인증 파일을 만들지 못했습니다. /tmp/wp_update 볼륨의 권한 또는 마운트 옵션을 확인하세요."
  fi
  docker exec -u 0 "$apache_container" chown root:www-data "$auth_file"
  docker exec -u 0 "$apache_container" chmod 640 "$auth_file"
fi

backup="${host_config}.bak.$(date +%Y%m%d%H%M%S)"
if grep -Fq "$config_marker" "$host_config"; then
  temporary_config="$(mktemp "${host_config}.tmp.XXXXXX")"
  cp -p -- "$host_config" "$backup"
  awk -v marker="$config_marker" -v ip="$moonfire_ip" '
    index($0, marker) { in_block = !in_block }
    in_block && /ProxyPass \/ http:\/\/[^:]+:8080\// {
      sub(/http:\/\/[^:]+:8080\//, "http://" ip ":8080/")
    }
    in_block && /ProxyPassReverse \/ http:\/\/[^:]+:8080\// {
      sub(/http:\/\/[^:]+:8080\//, "http://" ip ":8080/")
    }
    { print }
  ' "$host_config" > "$temporary_config"
  cat "$temporary_config" > "$host_config"
  echo "기존 CCTV 프록시 대상을 현재 Moonfire IP $moonfire_ip 로 갱신했습니다. 백업: $backup"
else
  temporary_config="$(mktemp "${host_config}.tmp.XXXXXX")"
  cat "$host_config" > "$temporary_config"
  cat >> "$temporary_config" <<APACHE_VHOST

# BEGIN rust_cctv CCTV HTTPS proxy
<VirtualHost *:443>
    ServerName $domain
    SSLEngine on
    SSLCertificateFile /etc/letsencrypt/live/$domain/fullchain.pem
    SSLCertificateKeyFile /etc/letsencrypt/live/$domain/privkey.pem

    ProxyRequests Off
    ProxyPreserveHost On
    ProxyPass / http://$moonfire_ip:8080/ connectiontimeout=5 timeout=300
    ProxyPassReverse / http://$moonfire_ip:8080/

    <Location />
        AuthType Basic
        AuthName "CCTV"
        AuthUserFile /tmp/wp_update/cctv.htpasswd
        Require valid-user
    </Location>

    ErrorLog \${APACHE_LOG_DIR}/cctv-error.log
    CustomLog \${APACHE_LOG_DIR}/cctv-access.log combined
</VirtualHost>
# END rust_cctv CCTV HTTPS proxy
APACHE_VHOST
  cp -p -- "$host_config" "$backup"
  cat "$temporary_config" > "$host_config"
  echo "Apache 설정에 백업을 만들었습니다: $backup"
fi

if ! docker exec "$apache_container" apache2ctl -t; then
  if [[ -f "$backup" ]]; then
    cp -p -- "$backup" "$host_config"
    echo "Apache 설정 검증에 실패해 변경을 백업본으로 복원했습니다." >&2
  fi
  fail "Apache 설정 검증에 실패했습니다. 오류 출력을 확인하세요."
fi

docker exec "$apache_container" apache2ctl graceful ||
  fail "설정은 유효하지만 Apache graceful 재시작에 실패했습니다."

echo
echo "완료: https://$domain 에 Basic 인증과 Moonfire NVR 프록시를 적용했습니다."
echo "인증서 자동 갱신 후 Apache를 다시 읽히도록 호스트의 갱신 작업도 별도로 확인하세요."
