#!/usr/bin/env python3

import base64
import json
import os
import sys
import urllib.error
import urllib.request


credentials = sys.stdin.read().splitlines()
if len(credentials) != 4:
    raise SystemExit("오류: 인증 정보를 모두 입력하지 못했습니다.")

apache_user, apache_password, moonfire_user, moonfire_password = credentials
cctv_url = os.environ.get("CCTV_URL", "")
nvr_direct_url = os.environ.get("NVR_DIRECT_URL", "http://127.0.0.1:8081")
if not cctv_url:
    raise SystemExit("오류: CCTV_URL 환경변수로 HTTPS 프록시 주소를 지정하세요.")


def test_login(label, url, username, password, basic_auth=None):
    headers = {"Content-Type": "application/json"}
    if basic_auth:
        raw = f"{basic_auth[0]}:{basic_auth[1]}".encode()
        headers["Authorization"] = "Basic " + base64.b64encode(raw).decode("ascii")

    request = urllib.request.Request(
        url.rstrip("/") + "/api/login",
        data=json.dumps({"username": username, "password": password}).encode(),
        headers=headers,
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=12) as response:
            status = response.status
    except urllib.error.HTTPError as error:
        status = error.code
        challenge = error.headers.get("WWW-Authenticate")
        if challenge:
            print(f"{label}: WWW-Authenticate: {challenge}")
    except (urllib.error.URLError, TimeoutError) as error:
        print(f"{label}: 연결 실패 ({error})")
        return None

    if status == 204:
        print(f"{label}: 성공 (HTTP 204, Moonfire 로그인 승인)")
    elif status == 401:
        if label == "직접 연결":
            print(f"{label}: HTTP 401 — Moonfire가 NVR 사용자명/비밀번호를 거부했습니다.")
        else:
            print(f"{label}: HTTP 401 — 프록시가 인증을 거부했습니다.")
    elif status == 403:
        print(f"{label}: HTTP 403 — Moonfire가 사용자명/비밀번호를 거부했습니다.")
    else:
        print(f"{label}: HTTP {status} — 예상된 로그인 응답(204/401/403)이 아닙니다.")
    return status


print("\n=== Moonfire 직접 연결 (localhost:8081) ===")
direct = test_login(
    "직접 연결",
    nvr_direct_url,
    moonfire_user,
    moonfire_password,
)

print("\n=== Apache HTTPS 프록시 ===")
proxied = test_login(
    "HTTPS 프록시",
    cctv_url,
    moonfire_user,
    moonfire_password,
    (apache_user, apache_password),
)

print("\n결과 해석: 직접 연결이 401/403이면 Moonfire 사용자 계정/비밀번호를 확인하세요.")
print("프록시에서 WWW-Authenticate: Basic이 표시되면 Apache Basic 인증이 거부된 것입니다.")
print("직접 연결은 204인데 프록시가 실패하면 Apache 프록시/Basic 인증 설정을 확인하세요.")
