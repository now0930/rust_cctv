# Rust CCTV

Moonfire NVR를 Docker로 실행하는 프로젝트 시작 구성입니다. Moonfire NVR 자체가 Rust로 작성되어 있고, 이 저장소의 Rust crate에는 카메라·감지 이벤트·저장소 기능을 따로 확장할 수 있는 모듈 골격이 있습니다. 별도 crate의 모듈은 현재 Moonfire NVR 바이너리와 연결되어 있지 않습니다.

## Moonfire NVR 실행

Docker Engine과 Docker Compose가 필요합니다. 컨테이너는 로컬 머신에서만 접근하도록 포트가 `127.0.0.1`에 바인딩되어 있습니다. 초기 DB를 만들고 서버를 실행하세요.

```sh
mkdir -p storage
PUID=$(id -u) PGID=$(id -g) docker compose pull
PUID=$(id -u) PGID=$(id -g) docker compose run --rm moonfire-nvr init
PUID=$(id -u) PGID=$(id -g) docker compose up -d
```

웹 UI: <http://localhost:8081>

카메라와 저장 디렉터리는 Moonfire NVR의 설정 TUI에서 추가합니다. 서버를 중지한 다음 아래 명령으로 설정을 열고, 끝나면 다시 시작하세요.

```sh
PUID=$(id -u) PGID=$(id -g) docker compose stop
PUID=$(id -u) PGID=$(id -g) docker compose run --rm moonfire-nvr config
PUID=$(id -u) PGID=$(id -g) docker compose up -d
```

호스트에 연결된 USB 웹캠은 `webcam-publisher`가 V4L2에서 읽어 RTSP로 변환하고, `webcam-rtsp`가 `rtsp://webcam-rtsp:8554/webcam` 주소로 제공합니다. `/dev/video0`와 호스트 `video` 그룹이 컨테이너에 전달됩니다. video 그룹 번호가 44가 아니면 `.env`에 `VIDEO_GID`를 지정하세요. Moonfire 설정의 **Cameras and streams**에서 이 RTSP URL을 추가하고 stream을 sample directory에 연결해 recording을 켜야 저장됩니다. 이 경로는 변환을 위해 CPU 인코딩을 사용합니다.

이 시작 설정은 인증 없이 영상 열람을 허용하므로 `8081`을 인터넷이나 신뢰할 수 없는 네트워크에 공개하지 마세요. 원격 접근에는 인증 및 HTTPS 프록시를 설정하세요. RTSP 카메라 비밀번호를 저장소에 커밋하지 마세요.

Moonfire NVR은 영상 저장에 H.264/RTSP를 사용하며, 모션 감지/영상 분석은 제공하지 않습니다. 설정과 데이터는 각각 `config/moonfire-nvr.toml`, `storage/`에 있습니다. 사용자 ID와 그룹 ID는 Compose 실행 때 `PUID`/`PGID` 환경변수로 호스트 사용자에 맞춰 전달합니다. 운영 상세: <https://github.com/scottlamb/moonfire-nvr/blob/v0.7.33/guide/install.md>

## CCTV HTTPS 프록시

`.env.example`을 참고해 서버 전용 `.env`를 만들고 도메인, Apache 컨테이너, 공용 Docker 네트워크, Apache 호스트 설정 파일, 영상 저장 경로를 지정하세요. `.env`는 Git에서 제외됩니다. 처음 설정할 때만 `cp .env.example .env`를 실행하고 실제 값으로 편집하세요. 셸 스크립트에도 `.env`의 값을 전달하려면 다음처럼 현재 셸에서 불러오세요.

```sh
set -a
. ./.env
set +a
./issue-cctv-cert.sh
./configure-cctv-apache.sh
```

프록시 설정 스크립트는 Apache 설정 파일을 백업하고 구문 검사에 통과한 경우에만 graceful 재시작합니다. Moonfire 컨테이너 IP를 프록시 대상으로 기록하므로 컨테이너를 재생성해 IP가 바뀌면 다시 실행해야 합니다.

Apache Basic 인증과 Moonfire NVR 로그인은 서로 별개입니다. NVR 웹 로그인 계정은 `./configure-moonfire-users.sh`를 실행하고 Moonfire 설정 화면의 **Users** 항목에서 추가하세요. 기존 사용자가 로그에 `incorrect password`로 표시되면 Users 항목에서 해당 사용자의 비밀번호를 재설정해야 합니다. 이 스크립트는 설정 편집 중 녹화가 중단되지 않도록 NVR을 잠시 중지하고, 설정 화면을 닫으면 다시 시작합니다. 설정 도구가 즉시 종료되거나 실패하면 스크립트가 임시 진단 로그를 출력합니다.

로그인이 계속 실패하면 `.env`를 셸에 불러온 뒤 `./test-moonfire-login.sh`를 실행해 두 인증 단계를 각각 확인할 수 있습니다. Apache와 Moonfire 비밀번호를 화면에 표시하지 않고 입력받으며, 계정 정보는 서버로만 전송하고 출력하지 않습니다.

인증서 갱신 뒤에도 Apache가 새 인증서를 사용하도록 호스트에서 Apache graceful reload가 실행되어야 합니다. Certbot 컨테이너 안의 `--post-hook`에서 `docker exec`를 실행하려면 Docker CLI와 Docker 소켓이 필요하므로, 갱신 작업이 실제로 reload까지 수행하는지 확인하세요.

## Rust 모듈

- `src/camera.rs`: 카메라 도메인 모델
- `src/detection.rs`: 감지 이벤트 모델
- `src/storage.rs`: 감지 이벤트 저장소 인터페이스

Rust toolchain 설치 후 `cargo run`으로 crate 골격을 실행할 수 있습니다. 필요한 기능에 맞춰 각 모듈의 구현과 Moonfire 연동을 추가하세요.
