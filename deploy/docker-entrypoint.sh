#!/bin/sh
# First start: creates /data/config.toml and the first access password. Later starts keep them.
set -eu

: "${RELAY_DOMAIN:?RELAY_DOMAIN 을 .env 에 넣어 주세요 (예: relay.mydomain.com)}"
: "${RELAY_EMAIL:?RELAY_EMAIL 을 .env 에 넣어 주세요}"

export AIC_RELAY_CONFIG=/data/config.toml
aic-relay init --domain "$RELAY_DOMAIN" --email "$RELAY_EMAIL" --data-dir /data
# Older versions opened the admin page on all interfaces for Docker's port mapping; with host
# networking that would expose it, so keep it on 127.0.0.1.
sed -i 's/^admin_bind = "0.0.0.0:7843"/admin_bind = "127.0.0.1:7843"/' /data/config.toml

cd /data
exec aic-relay run
