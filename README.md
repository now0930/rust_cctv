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

프록시 설정 스크립트는 Apache 설정 파일을 백업하고 구문 검사에 통과한 경우에만 graceful 재시작합니다. Moonfire 컨테이너 IP를 프록시 대상으로 기록하므로 컨테이너를 재생성해 IP가 바뀌면 다시 실행해야 합니다. 기존 Apache 설정에 남아 있는 Frigate 프록시 규칙도 스크립트가 제거합니다.

Apache Basic 인증과 Moonfire NVR 로그인은 서로 별개입니다. NVR 웹 로그인 계정은 `./configure-moonfire-users.sh`를 실행하고 Moonfire 설정 화면의 **Users** 항목에서 추가하세요. 기존 사용자가 로그에 `incorrect password`로 표시되면 Users 항목에서 해당 사용자의 비밀번호를 재설정해야 합니다. 이 스크립트는 설정 편집 중 녹화가 중단되지 않도록 NVR을 잠시 중지하고, 설정 화면을 닫으면 다시 시작합니다. 설정 도구가 즉시 종료되거나 실패하면 스크립트가 임시 진단 로그를 출력합니다.

로그인이 계속 실패하면 `.env`를 셸에 불러온 뒤 `./test-moonfire-login.sh`를 실행해 두 인증 단계를 각각 확인할 수 있습니다. Apache와 Moonfire 비밀번호를 화면에 표시하지 않고 입력받으며, 계정 정보는 서버로만 전송하고 출력하지 않습니다.

인증서 갱신 뒤에도 Apache가 새 인증서를 사용하도록 호스트에서 Apache graceful reload가 실행되어야 합니다. Certbot 컨테이너 안의 `--post-hook`에서 `docker exec`를 실행하려면 Docker CLI와 Docker 소켓이 필요하므로, 갱신 작업이 실제로 reload까지 수행하는지 확인하세요.

## Rust 모듈

- 구현 순서와 검증 기준: [ROADMAP.md](ROADMAP.md)
- `src/camera.rs`: 카메라 도메인 모델
- `src/detection.rs`: 감지 이벤트 모델
- `src/events.rs`: track 확정/종료 전이 이벤트 생성
- `src/storage.rs`: 감지 이벤트 저장소 인터페이스

`rust-cctv` 서비스 바이너리는 SQLite 이벤트 DB를 열고 무결성을 확인합니다. `RUST_CCTV_RTSP_URL` 또는 `--rtsp-url`을 지정하면 FFmpeg로 RTSP를 TCP 수신하고 320×180 gray8 프레임으로 축소해 차영상 지표를 출력합니다. 연결이 끊기면 1초부터 최대 30초까지 지수형 대기 후 재연결합니다. 로그에는 구간 FPS, 누적 디코드 프레임, 구간 평균/최대 gray 차이, 평균 축소·차영상 처리 시간, 임계값 초과 motion frame 수가 포함됩니다. `motion_frames`는 전체 320×180 평균 픽셀 차이가 5를 넘은 프레임 수입니다. `RUST_CCTV_MODEL_PATH` 또는 `--model-path`를 지정하면 Tiny YOLOv2 416×416 ONNX 모델로 person/dog를 추론합니다. 모델 입력은 RGB float32 0~255이며, 추론 대기 프레임은 최신 한 장만 유지해 오래된 프레임이 누적되지 않게 합니다. confidence 기본값은 0.25이고 `RUST_CCTV_SCORE_THRESHOLD` 또는 `--score-threshold`로 바꿀 수 있습니다. 5회 이상 검출된 track의 확정/종료 이벤트를 SQLite에 저장하고 5초마다 추론 FPS, 추론 시간, 프레임 나이, 이벤트 건수를 출력합니다. 프레임은 메모리에서만 처리하며 영상이나 스냅샷을 저장하지 않습니다. DB 기본 경로는 `./data/events.sqlite3`, camera ID 기본값은 `logitech_c922`입니다. `RUST_CCTV_DB_PATH`, `RUST_CCTV_CAMERA_ID` 환경변수 또는 `--db-path`, `--camera-id` 인자로 바꿀 수 있습니다. `SIGINT`/`SIGTERM`에서 정상 종료합니다. 모델 추론과 이벤트 저장의 실제 서비스 동작은 서버에서 확인해야 합니다.

```sh
RUST_CCTV_DB_PATH=./data/events.sqlite3 \
RUST_CCTV_CAMERA_ID=logitech_c922 \
RUST_CCTV_RTSP_URL=rtsp://127.0.0.1:8554/webcam \
RUST_CCTV_MODEL_PATH=/path/to/tinyyolov2-7.onnx \
cargo run --release --bin rust-cctv
```

Compose의 MediaMTX는 호스트 native Rust 서비스가 접근할 수 있도록 RTSP TCP 포트를 `127.0.0.1:8554`에만 게시합니다. Compose 설정을 적용하려면 `docker compose up -d webcam-rtsp`를 실행하세요. 사용자명/비밀번호가 포함된 RTSP URL은 셸 기록이나 로그에 남지 않게 관리하세요.

Rust toolchain과 FFmpeg 개발 라이브러리 설치 후 `cargo run`으로 서비스 스캐폴드를 실행할 수 있습니다. `rtsp-probe`는 RTSP 디코딩, 320×180 회색조 축소, 프레임 차이 계산만 하는 성능 실험용 실행 파일입니다. `model-probe`는 로컬 ONNX 파일의 추론 시간을 재는 별도 실행 파일입니다. 탐지 모델 파일은 저장소에 포함하지 않으며, 두 프로브 모두 영상 파일을 만들지 않습니다.

```sh
cargo run --release --bin rtsp-probe -- rtsp://camera-host:8554/stream 30 320 180
cargo run --release --bin model-probe -- /path/to/model.onnx 30 rtsp://camera-host:8554/stream
cargo run --release --bin model-probe -- /path/to/model.onnx 600 video:/tmp/replay.mkv
```

`model-probe`는 float32 NCHW 416×416 Tiny YOLOv2 모델에 RTSP URL을 지정하면 프레임을 메모리에서 변환해 추론하고 VOC person/dog 박스를 디코딩합니다. RTSP는 용량 1의 최신 프레임 슬롯을 사용해 처리 중 밀린 오래된 대기 프레임을 교체합니다. `video:/path/to/file.mkv`는 로컬 동영상을 순차 디코딩해 프레임을 건너뛰지 않고 같은 샘플을 반복 분석하며, `image:/path/to/image.jpg`는 정지 이미지를 분석합니다. 동영상 benchmark는 처음 3프레임으로 모델을 예열하고, 그 다음부터 지정한 iterations만큼 처리하거나 파일 끝에서 멈춥니다. 전체 구간의 person/dog 검출 프레임과 최대 점수를 집계하고, 클래스별 greedy IoU 추적으로 track ID와 hit 수를 표시합니다. 5회 이상 검출된 track은 `confirmed=true`로 구분하며 최근 종료된 track도 결과에 출력합니다. 확정된 track은 종료 뒤 최대 60 프레임 동안 보관하고 IoU 0.5 이상인 같은 클래스 박스가 돌아오면 같은 ID를 재사용합니다. `tracks_reidentified`가 재연결 횟수를 표시합니다. Track별 `active`는 실행 종료 시 아직 유지 중인지 표시합니다. `active_confirmed_tracks`는 실행 종료 시점에 활성인 확정 track, `confirmed_tracks_total`은 구간 전체에서 확정된 track 수입니다. RTSP benchmark는 활성 track에서 15회 미검출까지 ID를 유지합니다. 신뢰도 기준은 마지막 인자로 조정합니다(기본 0.25). 입력 픽셀은 RGB 순서의 float32 0~255 범위로 전달합니다. 영상은 임시 재현 시험에만 사용하고 저장소에 포함하지 않습니다. 입력은 416×416으로 직접 크기를 조정합니다. 공개 dog/person 샘플에서 각각 dog 0.751, person 0.649를 검출했고, 실제 카메라 앞 사람 시험에서 60회 중 53회 및 60회 검출된 구간을 확인했습니다. 이는 기능 확인이며, Pi 4 성능과 여러 장면의 정확도는 별도 평가가 필요합니다.
분석 코드의 `TrackEventEmitter`는 5회 이상 검출된 track에 `TrackConfirmed`, 재연결 유예가 지난 track에 `TrackLost` 이벤트를 만듭니다. 이벤트에는 실행 ID, 카메라/track ID, 클래스, confidence, 정규화 박스, Unix epoch 밀리초 시각을 담습니다. `InMemoryEventStore`는 최근 이벤트 최대 10,000개를 유지합니다. `SqliteEventStore`는 같은 이벤트를 SQLite에 쓰고 시간순 조회를 제공하며, WAL journal mode와 5초 busy timeout을 사용합니다. `model-probe`에서 `RUST_CCTV_EVENT_DB=/data/events.sqlite3`를 지정하면 DB에 기록하고, 저장 건수와 누적 행 수를 출력합니다. Docker에서 쓰려면 해당 DB 경로를 호스트 디렉터리에 bind mount하세요. 원본 영상과 스냅샷은 DB에 기록하지 않습니다.

`event-db` 명령으로 DB 무결성과 최근 이벤트를 읽기 전용으로 확인할 수 있습니다. 두 번째 인자는 표시할 최근 이벤트 수이며 기본값은 20입니다.

```sh
cargo run --release --bin event-db -- /path/to/events.sqlite3 20
```
