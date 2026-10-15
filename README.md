# AI Character 중계 서버 (aic-relay)

집에 있는 컴퓨터의 AI를 **카페·회사 같은 다른 곳에서** 쓸 때, 내 컴퓨터들끼리 이어 주는 서버입니다.
AI Character 앱과 짝을 이루는 별도 프로그램이고, 내 서버에 **한 번만** 설치해 두면 됩니다.

- 같은 공유기(와이파이) 안에서만 쓸 거라면 이 서버는 **필요 없습니다.**
- 대화 내용은 두 컴퓨터 사이에서 **처음부터 끝까지 암호화**됩니다. 이 서버는 내용을 볼 수 없고, 연결만 이어 줍니다.
- 가능하면 두 컴퓨터가 직접 연결되고, 직접 연결이 안 될 때만 서버를 거칩니다.
- **접속 암호**를 아는 기기만 이 서버를 쓸 수 있습니다. 암호는 여러 개 만들 수 있고, 서버를 끄지 않고 추가·삭제할 수 있습니다.
- **6자리 숫자로 연결**: 긴 연결 코드 대신 `482 917` 같은 숫자만 옮겨 적으면 됩니다. 숫자는 서버에 암호화된 채 10분 동안 한 번만 보관됩니다.
- **켜짐/꺼짐 표시**: 앱의 컴퓨터 카드에 그 컴퓨터가 지금 켜져 있는지(🟢/⚫)와 마지막 접속 시각이 나옵니다.
- **직접/중계 표시와 통계**: 연결이 두 컴퓨터 사이에서 바로 이어졌는지(⚡) 서버를 거치는지(🔁) 앱에 나오고, 관리 화면에 직접 연결 비율이 쌓입니다.
- **무차별 대입 차단**: 틀린 접속 암호·없는 6자리 숫자·틀린 관리자 암호를 10분 안에 20번 넘게 보낸 인터넷 주소는 30분 동안 막습니다.
- **휴대폰에서 쓰기**: 휴대폰 브라우저로 `https://relay.mydomain.com/app` 을 열면 앱 설치 없이 집 컴퓨터 AI와 대화할 수 있습니다 (대화는 휴대폰↔집 컴퓨터 사이에서 암호화, 서버는 못 봄).
- **관리 화면**에서 연결된 기기 수, 암호별 사용 현황, 직접 연결 비율, 막힌 주소(풀기 가능)를 볼 수 있습니다. 원하면 `https://도메인/admin/` 으로도 열 수 있습니다.

> 기술 메모: [iroh](https://iroh.computer) 1.3 의 relay 서버 라이브러리를 그대로 쓰고(앱과 같은 프로토콜),
> 앞단에 작은 HTTPS 서버를 두어 접속 암호 확인·주소 차단·앱 기능(6자리 숫자, 켜짐/꺼짐, 경로 통계)·휴대폰 페이지를 더했습니다.
> 중계 데이터는 앞단에서 손대지 않고 그대로 넘깁니다. 인증서는 Let's Encrypt 로 자동 발급·갱신됩니다.

---

## 0. 준비물

| 준비물 | 설명 |
|---|---|
| 서버 1대 | Ubuntu 22.04 / 24.04 (또는 Debian 12), x86_64 또는 ARM64. CPU 1개, 메모리 512MB 이상, 공인 IP. (AWS Lightsail, 오라클 클라우드 무료, Vultr, 집에 상시 켜 둔 리눅스 PC 등) |
| 도메인 | 가지고 계신 도메인에 `relay` 같은 이름을 하나 붙여 씁니다. 예) `relay.mydomain.com` |
| 이메일 | 무료 보안 인증서 안내 메일을 받을 주소 |

> 서버에서 이미 다른 웹사이트(nginx, Apache 등)가 80·443 포트를 쓰고 있다면 함께 쓸 수 없습니다. 중계 서버 전용 서버를 권합니다.

---

## 1. 도메인 연결 (DNS A 레코드)

도메인을 산 곳(가비아, 카페24, Cloudflare, 후이즈 등)의 **DNS 관리** 화면에서 레코드를 하나 추가합니다.

| 항목 | 넣을 값 |
|---|---|
| 종류(Type) | `A` |
| 이름(Host) | `relay` (→ `relay.mydomain.com`) |
| 값(Value) | 서버의 공인 IP (예: `203.0.113.10`) |

- **Cloudflare**: 주황색 구름(프록시)을 눌러 **회색(DNS only)** 으로 바꿔야 합니다.
  프록시가 켜져 있으면 인증서를 받지 못하고, 모든 접속자가 같은 주소로 보여 암호 대입을 막을 수 없습니다.
  설치 스크립트는 도메인이 이 서버를 가리키지 않으면 멈춥니다. 관리 화면에도 경고가 뜹니다.
- 반영까지 보통 5~30분. `ping relay.mydomain.com` 에 서버 IP가 나오면 준비된 것입니다.

## 2. 포트 열기

클라우드 서버라면 **관리 화면의 방화벽(보안 그룹 등)** 에서 아래 3개를 "들어오는 연결 허용"으로 추가하세요.
서버 안의 방화벽(ufw)은 설치 스크립트가 알아서 엽니다.

| 포트 | 프로토콜 | 용도 |
|---|---|---|
| 80 | TCP | 서버 확인용 |
| 443 | TCP | 중계 연결 (암호화) + 인증서 발급 |
| 7842 | UDP | 두 컴퓨터가 직접 연결되도록 돕기 |

관리 화면 포트(7843)는 **열지 마세요.** 서버 안에서만 열리고, SSH로 접속해서 봅니다(아래 5번).

---

## 3. 설치 (방법 A: 스크립트 — 추천)

이 폴더 전체를 서버로 복사한 뒤 스크립트를 실행합니다.

```bash
# 내 컴퓨터에서 (ubuntu 와 IP는 내 서버에 맞게)
scp -r AI-Character-Relay ubuntu@203.0.113.10:~/

# 서버에서
cd ~/AI-Character-Relay
sudo bash deploy/install.sh relay.mydomain.com me@mydomain.com
```

스크립트가 하는 일:

1. 도메인이 이 서버를 가리키는지 확인 (다른 IP를 가리키면 멈춤. 확실할 때만 `FORCE=1 sudo -E bash deploy/install.sh ...`)
2. 프로그램 준비: 폴더에 `aic-relay` 실행 파일이 있으면 그대로 쓰고, 없으면 소스로 빌드 (처음 한 번 5~15분, 메모리가 적으면 빌드하는 동안만 임시 스왑 사용)
3. 설정 파일(`/etc/aic-relay/config.toml`)과 첫 접속 암호, 관리자 암호 생성
4. 서버가 켜질 때 자동 실행 등록 (systemd)
5. 보안 인증서 자동 발급 (만료 전 자동 갱신)

끝나면 이렇게 나옵니다. **메모해 두세요.**

```
 중계 서버 주소 : relay.mydomain.com
 접속 암호      : (32자리)
 관리자 암호    : (24자리)
```

> 빌드된 실행 파일을 쓰고 싶다면: GitHub 에 올린 뒤 `v0.1.0` 같은 태그를 만들면 `.github/workflows/release.yml` 이
> `aic-relay-x86_64-linux.tar.gz` / `aic-relay-aarch64-linux.tar.gz` 를 만들어 줍니다. 압축을 풀고 그 안에서
> `sudo bash deploy/install.sh ...` 를 실행하면 빌드 없이 바로 설치됩니다.

## 3. 설치 (방법 B: Docker)

```bash
cd AI-Character-Relay
cp .env.example .env
nano .env                         # RELAY_DOMAIN, RELAY_EMAIL 채우기
docker compose up -d --build
docker compose logs relay | head  # 접속 암호와 관리자 암호 확인
```

- Linux 서버 전용입니다. `network_mode: host` 로 실행해 서버가 80·443·7842 포트를 직접 엽니다.
  (Docker 포트 중계를 거치면 접속자 주소가 모두 Docker 내부 주소로 보여서 암호 대입을 막을 수 없기 때문입니다.)
- 관리 화면은 서버의 127.0.0.1:7843 에만 열리므로 아래 5번처럼 SSH 터널로 봅니다.

---

## 4. 앱에 등록

**AI를 빌려주는 컴퓨터(집 컴퓨터)** 에서만 하면 됩니다.

1. AI Character 메뉴 → **처음 설정 (웹)** → **🌐 다른 컴퓨터 AI**
2. **🌍 밖에서도 쓰기: 중계 서버** 카드에 중계 서버 주소와 접속 암호를 넣고 **저장 → 연결 확인**
3. 같은 화면의 **A. 이 컴퓨터를 빌려주기 → 연결 코드 만들기** → 크게 나온 **6자리 숫자**를 노트북의 **B. 다른 컴퓨터 연결하기** 에 입력

- 6자리 숫자를 쓰려면 노트북에도 같은 중계 서버(주소·접속 암호)를 저장해 두거나, B 카드의 '중계 서버 정보 직접 넣기'에 한 번 넣으면 됩니다.
- 숫자 대신 **긴 연결 코드**를 복사해 붙여 넣어도 됩니다. 긴 코드에는 중계 서버 정보가 들어 있어 노트북 설정이 필요 없습니다.
- 중계 서버를 나중에 설정했다면, 이미 연결한 노트북도 **연결 코드를 새로 만들어 한 번 더 연결**하세요.

## 4-1. 휴대폰에서 쓰기

1. 휴대폰 브라우저(Safari, Chrome)로 **https://relay.mydomain.com/app** 을 엽니다.
2. **접속 암호**를 넣습니다 (처음 한 번만, 휴대폰에 저장됨).
3. 집 컴퓨터에서 **A. 이 컴퓨터를 빌려주기 → 연결 코드 만들기** 를 누르고, 나온 **6자리 숫자**를 휴대폰에 넣습니다.
4. 모델을 고르고 대화합니다. 공유 메뉴의 **홈 화면에 추가**를 하면 앱처럼 쓸 수 있습니다.

- 휴대폰 브라우저는 직접 연결을 못 해서 항상 서버를 거치지만, 내용은 휴대폰과 집 컴퓨터 사이에서 암호화되어 서버가 볼 수 없습니다.
- 휴대폰을 잃어버리면 집 컴퓨터의 **A** 카드 목록에서 그 휴대폰을 **연결 끊기** 하세요.
- 이 페이지가 필요 없으면 `/etc/aic-relay/config.toml` 에 `phone_web = false` 를 넣고 다시 시작하세요.

---

## 5. 관리

### 관리 화면 (브라우저)

관리 화면은 보안을 위해 서버 안(127.0.0.1:7843)에서만 열립니다. 내 컴퓨터에서 SSH로 길을 만들어 봅니다.

```bash
ssh -L 7843:127.0.0.1:7843 ubuntu@relay.mydomain.com
```

이 창을 열어 둔 채 브라우저에서 **http://localhost:7843** → 아이디는 아무거나, 비밀번호는 관리자 암호.

- 지금 연결된 기기 수, 가동 시간, 누적 허용·거절 수
- 직접 연결 비율 (24시간·누적), 6자리 숫자 대기·사용 수
- 막힌 주소 목록과 **풀기** 버튼
- 접속 암호 목록 (보기·복사·지우기), 새 암호 만들기

**SSH 없이 브라우저로 바로 열고 싶다면** `/etc/aic-relay/config.toml` 에 `https_admin = true` 를 넣고
`sudo systemctl restart aic-relay` → **https://relay.mydomain.com/admin/** (관리자 암호를 여러 번 틀리면 그 주소는 30분 막힘).
관리자 암호를 길게 유지하세요. 기본은 꺼져 있습니다.

### 명령어

```bash
sudo aic-relay status               # 상태
sudo aic-relay token list           # 접속 암호 목록
sudo aic-relay token add 부모님집   # 새 접속 암호 (서버를 끄지 않아도 바로 적용)
sudo aic-relay token show 부모님집  # 암호 다시 보기
sudo aic-relay token remove 회사    # 지우기 (새 연결부터 막힘)
sudo aic-relay unblock 203.0.113.7  # 암호를 여러 번 틀려 막힌 주소 풀기
sudo systemctl restart aic-relay    # 다시 시작 (지운 암호로 연결 중인 기기도 바로 끊김)
sudo journalctl -u aic-relay -n 50  # 최근 기록
curl https://relay.mydomain.com/ping  # 200 이면 정상
```

- 관리자 암호는 `/etc/aic-relay/config.toml` 의 `admin_password` 입니다. 바꾸고 `systemctl restart aic-relay`.
- 막힌 주소는 `/var/lib/aic-relay/blocks.json` 에 저장되어 서버를 다시 켜도 30분이 지날 때까지 유지됩니다.
- Let's Encrypt 대신 **내 인증서**를 쓰려면 설정에 `tls_cert = "/경로/fullchain.pem"`, `tls_key = "/경로/privkey.pem"` 를 함께 넣으세요
  (443 포트로 발급하기 어려운 환경 등). 인증서를 바꾼 뒤에는 다시 시작해야 합니다.
- 접속 암호를 가족·장소별로 따로 만들면, 하나가 새어도 그것만 지우면 됩니다.

### 업데이트

새 소스(또는 새 실행 파일)를 서버에 복사하고 `sudo bash deploy/install.sh <도메인> <이메일>` 을 다시 실행하세요.
설정·접속 암호·인증서는 그대로 유지됩니다.

### 삭제

```bash
sudo systemctl disable --now aic-relay
sudo rm -rf /etc/aic-relay /var/lib/aic-relay /usr/local/bin/aic-relay /etc/systemd/system/aic-relay.service
sudo userdel aic-relay
```

---

## 자주 막히는 곳

| 증상 | 해결 |
|---|---|
| 스크립트가 "아직 어떤 서버도 가리키지 않습니다" | DNS가 아직 반영 안 됨. 10~30분 뒤 다시 실행 |
| `curl https://.../ping` 이 안 됨 | 클라우드 방화벽에서 80·443 이 열렸는지, Cloudflare 프록시가 꺼졌는지 확인. `journalctl` 에 `acme` 오류가 보이면 443 포트 문제 |
| 앱의 "연결 확인" 실패 | 주소·접속 암호 오타 확인. `sudo aic-relay status` 의 '거절한 연결' 이 늘면 암호가 틀린 것 |
| 연결은 되는데 느림 | 7842/udp 를 열면 직접 연결 비율이 올라가 빨라집니다. 관리 화면의 '직접 연결 비율'로 확인 |
| 갑자기 아무것도 안 됨 (429) | 틀린 암호를 여러 번 보내 내 인터넷 주소가 막혔을 수 있습니다. `sudo aic-relay unblock <내 IP>` 또는 30분 기다리기 |
| 휴대폰 페이지에서 "숫자를 찾지 못했습니다" | 숫자는 10분 동안 한 번만 쓸 수 있습니다. 집 컴퓨터에서 새로 만들어 주세요 |
| 관리 화면에 "프록시나 Docker 포트 중계가 있어" 경고 | Cloudflare 주황 구름을 회색으로, nginx 등 앞단 프록시를 빼고, Docker 는 `network_mode: host` 로 실행. 이 상태에서는 모두가 함께 막히지 않도록 암호 대입 차단을 일부러 꺼 둡니다 |
| 휴대폰을 잠깐 껐다 켜니 대화가 끊김 | 휴대폰은 백그라운드 탭을 멈춥니다. 돌아오면 자동으로 새로 연결하니 다시 보내 주세요 |
| 인증서 발급 한도 초과 | Let's Encrypt 는 같은 도메인에 일주일 5번까지 새로 발급. 재설치해도 받아 둔 인증서를 다시 쓰니 `/var/lib/aic-relay/certs` 를 지우지 마세요 |

## 보안 메모

- 이 서버는 암호화된 데이터를 전달만 하며 대화 내용·API 키를 볼 수 없습니다.
- 접속 암호는 앱의 연결 코드에도 들어갑니다. 연결 코드는 내 컴퓨터끼리만 주고받으세요.
- 내 AI를 쓸 수 있는 건 연결 코드로 허락한 컴퓨터뿐입니다. 접속 암호가 새어도 남이 내 AI를 쓸 수는 없고 이 서버만 쓸 수 있게 되니, 그 암호를 지우세요.
- 6자리 숫자는 짧은 대신 ① 접속 암호가 있어야만 조회할 수 있고 ② 한 번 쓰면 지워지며 ③ 10분 뒤 사라지고 ④ 틀린 숫자를 계속 보내면 그 주소가 막힙니다.
  서버는 숫자 자체를 받지 않고 숫자에서 만든 조회용 값만 받습니다. (서버 주인은 마음먹으면 맡겨진 내용을 풀 수 있으니, 남의 서버가 아니라 내 서버를 쓰세요.)
- 켜짐/꺼짐은 그 컴퓨터의 고유 번호(긴 공개 키)를 아는, 즉 연결해 본 기기만 조회할 수 있습니다. 기록은 메모리에만 있어 서버를 다시 켜면 사라집니다.
- 휴대폰 페이지는 서버가 보내 주는 프로그램이므로 이 서버를 믿을 수 있어야 합니다 (내 서버라면 문제없음).

## 개발

```bash
cargo test           # 실제 중계 서버를 띄워 앱과 같은 방식으로 연결·암호 변경을 확인
                     # (개발 모드 + 운영 모드: 자체 서명 인증서로 HTTPS·QUIC·TLS 중계까지)
cargo build --release
```

로컬 시험: `config.toml` 에 `dev = true`, `http_bind = "127.0.0.1:3340"` 을 넣으면 인증서 없이 http 로 실행됩니다
(앱의 중계 서버 주소에 `http://localhost:3340`, 휴대폰 페이지는 `http://localhost:3340/app/`).

휴대폰 페이지를 Ollama 없이 시험하려면 가짜 집 컴퓨터를 띄우세요:

```bash
cargo run --example fake_home -- http://localhost:3340 <접속 암호>   # 긴 연결 코드를 출력
```

진짜 앱의 공유 기능과 시험하려면 (앱 저장소의 `src-tauri` 에서, 앱 설정은 임시 폴더에 저장됨):

```bash
CARGO_HOME=~/.cargo RUSTUP_HOME=~/.rustup HOME=$(mktemp -d) AIC_TEST_RELAY=http://localhost:3340 AIC_TEST_RELAY_TOKEN=<접속 암호> \
  cargo test phone_pairs -- --ignored --nocapture   # SHORT_CODE= 로 나온 6자리를 휴대폰 페이지에 입력
```

### 휴대폰 페이지 다시 빌드

`web/` 은 브라우저용 iroh(WebAssembly) 클라이언트입니다. 빌드 결과(`web/dist/`)는 저장소에 들어 있고
`aic-relay` 실행 파일 안에 그대로 포함되므로, 서버만 빌드할 때는 아래가 필요 없습니다. `web/` 을 고쳤을 때만:

```bash
rustup target add wasm32-unknown-unknown
rustup component add llvm-tools          # macOS: wasm 용 llvm-ar
cargo install wasm-bindgen-cli --version <web/Cargo.lock 의 wasm-bindgen 버전>
web/build.sh                             # web/dist/ 갱신 → cargo build
```

| 파일 | 내용 |
|---|---|
| `src/main.rs` | 명령어 (`init`, `run`, `status`, `token`, `unblock`) |
| `src/lib.rs` | 전체 구성: 인증서(ACME 또는 파일), 내부 iroh relay, QUIC, 앞단 서버, 관리 화면 |
| `src/front.rs` | 공개 HTTP(S) 서버: 접속 암호 확인, 중계 전달, `/aic/v1/*` 앱 API, `/admin/`, `/app/` |
| `src/gate.rs` | 접속 암호 확인, 파일 변경 자동 반영, 연결 통계, 켜짐/꺼짐 |
| `src/guard.rs` | 주소별 실패 횟수와 차단 (차단은 `blocks.json` 에 저장, 사설 주소는 세지 않고 경고) |
| `src/hub.rs` | 6자리 숫자 보관함, 직접/중계 통계 |
| `src/web.rs` | 휴대폰 페이지 파일 제공 |
| `src/admin.rs`, `src/admin.html` | 관리 화면과 API (Basic 인증, CSRF 방지 헤더) |
| `web/` | 휴대폰 페이지 (Rust→WebAssembly + `static/`) |
| `examples/fake_home.rs` | 시험용 가짜 집 컴퓨터 |
| `deploy/` | 설치 스크립트, systemd, Docker 시작 스크립트 |
| `tests/relay.rs` | 통합 테스트 |

### 앱 API (`/aic/v1/`, 모두 `Authorization: Bearer <접속 암호>` 필요)

| 요청 | 내용 |
|---|---|
| `GET /info` | 버전, 지원 기능 |
| `POST /codes` `{id, sealed, minutes}` | 6자리 숫자 상자 맡기기 (id = SHA-256("aic-short-id/1:"+숫자), sealed = AES-256-GCM) |
| `POST /codes/take` `{id}` | 한 번 꺼내기 (없으면 404, 실패로 셈) |
| `POST /presence` `{ids:[...]}` | 켜짐/꺼짐과 마지막 접속 시각 |
| `POST /paths` `{path:"direct"\|"relay"}` | 연결 경로 보고 (통계용) |
