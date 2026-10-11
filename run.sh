#!/bin/bash

echo '=== Apache 명령어 위치 ==='
command -v apache2ctl apachectl httpd apache2 docker podman
echo '=== Apache 프로세스 ==='
ps -ef | grep -E '[a]pache2|[h]ttpd'
echo '=== 80/443 포트를 듣는 프로세스 ==='
ss -ltnp | grep -E ':(80|443)\b'
echo '=== 실행 중인 Docker 컨테이너 ==='
docker ps --format 'table {{.Names}}\t{{.Image}}\t{{.Ports}}'
echo '=== Apache 설정 파일 위치 ==='
find /etc /usr/local/etc -type f \( -name apache2.conf -o -name httpd.conf \) -print 2>/dev/null
