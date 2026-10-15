#!/usr/bin/env bash
# AI Character 중계 서버 설치 (Ubuntu 22.04+/Debian 12, x86_64 · ARM64)
#
#   사용법: sudo bash deploy/install.sh <도메인> <이메일>
#   예시:   sudo bash deploy/install.sh relay.mydomain.com me@mydomain.com
#
# 프로그램 파일(aic-relay)이 이 스크립트 옆이나 저장소 맨 위에 있으면 그대로 쓰고,
# 없으면 이 저장소의 소스로 직접 빌드합니다 (처음 한 번 5~15분).
# 다시 실행해도 됩니다. 접속 암호와 관리자 암호는 그대로 유지됩니다.
set -euo pipefail

DOMAIN="${1:-}"
EMAIL="${2:-}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CONFIG=/etc/aic-relay/config.toml
DATA=/var/lib/aic-relay
BIN=/usr/local/bin/aic-relay

say() { printf '\n\033[1;36m▶ %s\033[0m\n' "$1"; }
fail() { printf '\n\033[1;31m✖ %s\033[0m\n' "$1" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] || fail "관리자 권한이 필요합니다. 앞에 sudo 를 붙여 실행하세요."
[ -n "$DOMAIN" ] && [ -n "$EMAIL" ] || fail "도메인과 이메일을 넣어 주세요. 예) sudo bash deploy/install.sh relay.mydomain.com me@mydomain.com"
[[ "$DOMAIN" =~ ^[A-Za-z0-9.-]+$ ]] || fail "도메인에는 영문·숫자·점(.)·하이픈(-)만 쓸 수 있습니다: $DOMAIN"
[[ "$EMAIL" =~ ^[^[:space:]@]+@[^[:space:]@]+$ ]] || fail "이메일 형식이 아닙니다: $EMAIL"

say "필요한 도구 확인"
if ! command -v curl >/dev/null; then
  apt-get update -y && apt-get install -y curl ca-certificates
fi

say "도메인 연결 확인 ($DOMAIN)"
SERVER_IP="$(curl -4 -fsS --max-time 5 https://api.ipify.org || true)"
DOMAIN_IP="$(getent ahostsv4 "$DOMAIN" | awk 'NR==1 {print $1}' || true)"
if [ -z "$DOMAIN_IP" ]; then
  fail "$DOMAIN 이(가) 아직 어떤 서버도 가리키지 않습니다. 도메인 관리 화면에서 A 레코드를 이 서버 IP(${SERVER_IP:-확인 실패})로 추가하고, 5~30분 뒤 다시 실행하세요."
elif [ -n "$SERVER_IP" ] && [ "$DOMAIN_IP" != "$SERVER_IP" ]; then
  echo "⚠ $DOMAIN → $DOMAIN_IP 인데 이 서버 IP는 $SERVER_IP 입니다."
  echo "  이대로면 HTTPS 인증서를 받지 못하고, 암호를 계속 틀리는 주소도 막을 수 없습니다."
  echo "  - Cloudflare 주황 구름(프록시)이 켜져 있다면 회색(DNS only)으로 바꿔 주세요."
  echo "  - 방금 A 레코드를 바꿨다면 5~30분 뒤 다시 실행하세요."
  [ "${FORCE:-}" = "1" ] || fail "확실히 이 서버가 맞다면 FORCE=1 sudo -E bash deploy/install.sh ... 로 그대로 진행할 수 있습니다."
  echo "  FORCE=1 이므로 그대로 계속합니다."
else
  echo "✓ $DOMAIN → $DOMAIN_IP (이 서버)"
fi

say "프로그램 준비"
PREBUILT=""
for candidate in "$SCRIPT_DIR/aic-relay" "$REPO_DIR/aic-relay"; do
  [ -f "$candidate" ] && [ -x "$candidate" ] && PREBUILT="$candidate" && break
done
if [ -n "$PREBUILT" ]; then
  echo "받아 둔 프로그램 사용: $PREBUILT"
  install -m 0755 "$PREBUILT" "$BIN"
else
  [ -f "$REPO_DIR/Cargo.toml" ] || fail "프로그램 파일도 소스도 없습니다. 저장소 전체를 서버에 복사했는지 확인하세요."
  echo "소스로 빌드합니다 (처음 한 번 5~15분 걸립니다)."
  apt-get update -y && apt-get install -y build-essential pkg-config curl ca-certificates
  if ! command -v cargo >/dev/null && [ ! -x /root/.cargo/bin/cargo ]; then
    curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal
  fi
  export PATH="/root/.cargo/bin:$PATH"
  rustup update stable >/dev/null 2>&1 || true
  SWAP=""
  MEM_MB="$(awk '/MemTotal/ {print int($2/1024)}' /proc/meminfo)"
  if [ "$MEM_MB" -lt 2000 ] && [ ! -f /aic-build.swap ]; then
    echo "메모리가 ${MEM_MB}MB라 빌드하는 동안만 임시 스왑 2GB를 씁니다."
    fallocate -l 2G /aic-build.swap 2>/dev/null || dd if=/dev/zero of=/aic-build.swap bs=1M count=2048 status=none
    chmod 600 /aic-build.swap && mkswap /aic-build.swap >/dev/null && swapon /aic-build.swap && SWAP=1
  fi
  (cd "$REPO_DIR" && cargo build --release --locked)
  if [ -n "$SWAP" ]; then swapoff /aic-build.swap && rm -f /aic-build.swap; fi
  install -m 0755 "$REPO_DIR/target/release/aic-relay" "$BIN"
fi
echo "✓ $("$BIN" version)"

say "실행 계정과 설정"
id -u aic-relay >/dev/null 2>&1 || useradd --system --home-dir "$DATA" --shell /usr/sbin/nologin aic-relay
install -d -m 0750 -o root -g aic-relay /etc/aic-relay
install -d -m 0750 "$DATA"
"$BIN" init --config "$CONFIG" --domain "$DOMAIN" --email "$EMAIL" --data-dir "$DATA" >/dev/null
chown root:aic-relay "$CONFIG" && chmod 0640 "$CONFIG"
chown -R aic-relay:aic-relay "$DATA"

say "자동 실행 등록 (systemd)"
install -m 0644 "$SCRIPT_DIR/aic-relay.service" /etc/systemd/system/aic-relay.service
systemctl daemon-reload
systemctl enable aic-relay >/dev/null
systemctl restart aic-relay

say "방화벽"
if command -v ufw >/dev/null && ufw status | grep -q "Status: active"; then
  ufw allow 80/tcp >/dev/null && ufw allow 443/tcp >/dev/null && ufw allow 7842/udp >/dev/null
  echo "✓ ufw: 80/tcp, 443/tcp, 7842/udp 열림"
else
  echo "서버 안의 방화벽(ufw)은 꺼져 있습니다."
fi
echo "※ 클라우드(AWS·오라클·GCP·Lightsail 등) 관리 화면의 '보안 그룹/방화벽'에서도 80/tcp, 443/tcp, 7842/udp 를 열어야 합니다."

say "작동 확인 (인증서 발급에 최대 1분)"
OK=""
for _ in $(seq 1 20); do
  if curl -fsS --max-time 5 "https://$DOMAIN/ping" >/dev/null 2>&1; then OK=1; break; fi
  sleep 3
done
if [ -n "$OK" ]; then
  echo "✓ https://$DOMAIN 응답 확인"
else
  echo "⚠ 아직 https 응답이 없습니다. 1~2분 뒤 'curl https://$DOMAIN/ping' 으로 다시 확인하세요."
  echo "  안 되면: sudo journalctl -u aic-relay -n 50"
fi

FIRST_NAME="$("$BIN" token list --config "$CONFIG" | awk -F'\t' 'NR==1 {print $1}')"
TOKEN="$("$BIN" token show "$FIRST_NAME" --config "$CONFIG")"
ADMIN_PW="$(awk -F'"' '/^admin_password/ {print $2}' "$CONFIG")"
cat <<EOF

========================================================
 설치 완료! AI Character 앱에 아래 두 값을 넣으세요.
   (처음 설정 웹 → 🌐 다른 컴퓨터 AI → '밖에서도 쓰기: 중계 서버')

   중계 서버 주소 : $DOMAIN
   접속 암호      : $TOKEN

 휴대폰에서 쓰기: 휴대폰 브라우저로  https://$DOMAIN/app
   (접속 암호를 넣고, 집 컴퓨터에서 만든 6자리 숫자를 넣으면 됩니다)

 관리 화면 (상태 보기, 접속 암호 추가·삭제):
   내 컴퓨터에서  ssh -L 7843:127.0.0.1:7843 <서버 사용자>@$DOMAIN
   그다음 브라우저로  http://localhost:7843
   (아이디는 아무거나, 관리자 암호: $ADMIN_PW)

 명령으로 관리:  sudo aic-relay status / sudo aic-relay token list
========================================================
EOF
