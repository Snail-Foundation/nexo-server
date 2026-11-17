#!/usr/bin/env bash
#
# Cross-project integration check for the restaurant MVP.
#
# Starts a throwaway nexo-server, runs the nexo-restaurant example against it,
# and verifies the server projections and the waiter web app. Both projects
# keep their own Cargo manifests and are only connected through the public HTTP
# protocol.
#
# Usage:
#   nexo-server/scripts/restaurant-e2e.sh
#
# Environment:
#   NEXO_E2E_PORT          server port (default 18090)
#   NEXO_RESTAURANT_DIR    desktop checkout (default ../nexo-restaurant)
#   NEXO_E2E_KEEP=1        keep the temporary directory for inspection

set -euo pipefail

SERVER_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESTAURANT_DIR="${NEXO_RESTAURANT_DIR:-$(cd "$SERVER_DIR/../nexo-restaurant" && pwd)}"
PORT="${NEXO_E2E_PORT:-18090}"
BASE_URL="http://127.0.0.1:$PORT"
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/nexo-e2e.XXXXXX")"
COOKIES="$WORKDIR/cookies.txt"
SERVER_PID=""

fail() {
  echo "e2e: $*" >&2
  exit 1
}

cleanup() {
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi

  if [[ "${NEXO_E2E_KEEP:-0}" == "1" ]]; then
    echo "e2e: kept work directory $WORKDIR"
  else
    rm -rf "$WORKDIR"
  fi
}

trap cleanup EXIT

[[ -d "$RESTAURANT_DIR" ]] || fail "nexo-restaurant not found at $RESTAURANT_DIR (set NEXO_RESTAURANT_DIR)"
command -v curl >/dev/null || fail "curl is required"
command -v jq >/dev/null || fail "jq is required"
command -v sqlite3 >/dev/null || fail "sqlite3 is required"

if curl -fsS "$BASE_URL/api/health" >/dev/null 2>&1; then
  fail "port $PORT already has a server (set NEXO_E2E_PORT)"
fi

echo "e2e: building nexo-server and the desktop example"
SERVER_TARGET="$(cargo metadata --format-version 1 --no-deps --manifest-path "$SERVER_DIR/Cargo.toml" | jq -r .target_directory)"
RESTAURANT_TARGET="$(cargo metadata --format-version 1 --no-deps --manifest-path "$RESTAURANT_DIR/Cargo.toml" | jq -r .target_directory)"

cargo build --quiet --manifest-path "$SERVER_DIR/Cargo.toml"
cargo build --quiet --manifest-path "$RESTAURANT_DIR/Cargo.toml" --example server_e2e

echo "e2e: starting nexo-server on $BASE_URL"
NEXO_SERVER_DB="sqlite://$WORKDIR/server.db?mode=rwc" \
NEXO_SERVER_ADDR="127.0.0.1:$PORT" \
NEXO_ADMIN_PIN=1234 \
NEXO_BUSINESS_NAME="Nexo E2E" \
"$SERVER_TARGET/debug/nexo-server" >"$WORKDIR/server.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 100); do
  if curl -fsS "$BASE_URL/api/health" >/dev/null 2>&1; then
    break
  fi

  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    cat "$WORKDIR/server.log" >&2
    fail "the server exited before becoming healthy"
  fi

  sleep 0.1
done

curl -fsS "$BASE_URL/api/health" >/dev/null || {
  cat "$WORKDIR/server.log" >&2
  fail "the server did not become healthy"
}

echo "e2e: running the desktop flow"
NEXO_E2E_SERVER_URL="$BASE_URL" \
NEXO_E2E_DB="$WORKDIR/desktop.db" \
NEXO_E2E_SUMMARY="$WORKDIR/summary.json" \
NEXO_E2E_ADMIN_NAME="Ana" \
NEXO_E2E_ADMIN_PIN="2468" \
"$RESTAURANT_TARGET/debug/examples/server_e2e" >"$WORKDIR/desktop.log"

jq -e . "$WORKDIR/summary.json" >/dev/null

ORDER_ID="$(jq -r .order_id "$WORKDIR/summary.json")"
SALE_NUMBER="$(jq -r .sale_number "$WORKDIR/summary.json")"
TOTAL_MINOR="$(jq -r .total_minor "$WORKDIR/summary.json")"
DISPLAY_CODE="$(jq -r .display_code "$WORKDIR/summary.json")"

[[ -n "$ORDER_ID" && "$ORDER_ID" != "null" ]] || fail "missing order id"
[[ -n "$SALE_NUMBER" && "$SALE_NUMBER" != "null" ]] || fail "missing sale number"
[[ -n "$DISPLAY_CODE" && "$DISPLAY_CODE" != "null" ]] || fail "missing display code"
[[ "$(jq -r .order_status "$WORKDIR/summary.json")" == "billed" ]] \
  || fail "the desktop did not bill the order"
[[ "$(jq -r .sale_status "$WORKDIR/summary.json")" == "completed" ]] \
  || fail "the desktop sale is not completed"
[[ "$(jq -r .skipped "$WORKDIR/summary.json")" == "0" ]] \
  || fail "the desktop skipped remote operations"
[[ "$(jq -r .outbox_pending "$WORKDIR/summary.json")" == "0" ]] \
  || fail "the desktop outbox is not empty after synchronization"

read_db() {
  sqlite3 "$WORKDIR/server.db" "$1"
}

[[ "$(read_db "SELECT status FROM restaurant_order WHERE id = '$ORDER_ID';")" == "billed" ]] \
  || fail "the server did not project the order as billed"

[[ "$(read_db "SELECT number FROM sale WHERE order_id = '$ORDER_ID';")" == "$SALE_NUMBER" ]] \
  || fail "the server sale number does not match the desktop"

[[ "$(read_db "SELECT total_minor FROM sale WHERE order_id = '$ORDER_ID';")" == "$TOTAL_MINOR" ]] \
  || fail "the server sale total does not match the desktop"

[[ "$(read_db "SELECT COUNT(*) FROM sale_line WHERE sale_id = (SELECT id FROM sale WHERE order_id = '$ORDER_ID');")" == "1" ]] \
  || fail "the server did not project the sale line"

[[ "$(read_db "SELECT COUNT(*) FROM payment WHERE sale_id = (SELECT id FROM sale WHERE order_id = '$ORDER_ID');")" == "1" ]] \
  || fail "the server did not project the payment"

[[ "$(read_db "SELECT status FROM cash_session LIMIT 1;")" == "open" ]] \
  || fail "the server did not project the open cash session"

[[ "$(read_db "SELECT COUNT(*) FROM cash_movement WHERE cash_session_id = (SELECT id FROM cash_session LIMIT 1);")" == "1" ]] \
  || fail "the server did not project the sale cash movement"

[[ "$(read_db "SELECT COUNT(*) FROM operation_log WHERE kind IN ('sale_created', 'order_billed', 'cash_movement_recorded');")" == "3" ]] \
  || fail "the server is missing billing operations"

[[ "$(read_db "SELECT COUNT(*) FROM sale WHERE order_id = '$ORDER_ID' AND subtotal_minor + tax_minor = total_minor;")" == "1" ]] \
  || fail "the server sale totals are inconsistent"

[[ "$SALE_NUMBER" == V-* ]] || fail "the sale number does not use the V-#### format"

[[ "$(read_db "SELECT COUNT(*) FROM sync_anomaly;")" == "0" ]] \
  || fail "the server recorded synchronization anomalies"

OPERATIONS="$(read_db "SELECT COUNT(*) FROM operation_log;")"
(( OPERATIONS > 0 )) || fail "the server did not receive operations"

DESKTOP_PENDING="$(sqlite3 "$WORKDIR/desktop.db" "SELECT COUNT(*) FROM outbox WHERE status IN ('pending', 'failed');")"
[[ "$DESKTOP_PENDING" == "0" ]] || fail "the desktop outbox still has pending operations"

DESKTOP_CURSOR="$(sqlite3 "$WORKDIR/desktop.db" "SELECT last_seq FROM sync_cursor ORDER BY last_seq DESC LIMIT 1;")"
[[ "$DESKTOP_CURSOR" == "$OPERATIONS" ]] \
  || fail "the desktop cursor ($DESKTOP_CURSOR) does not match the server log ($OPERATIONS)"

echo "e2e: verifying the waiter web app"
LOGIN_STATUS="$(curl -sS -o /dev/null -w '%{http_code}' -c "$COOKIES" \
  --data-urlencode 'user=Ana' \
  --data-urlencode 'pin=2468' \
  "$BASE_URL/login")"

[[ "$LOGIN_STATUS" == "303" ]] || fail "web login returned $LOGIN_STATUS instead of 303"

curl -fsS -b "$COOKIES" "$BASE_URL/app/orders/$ORDER_ID" -o "$WORKDIR/order.html"

grep -q 'Cobrado' "$WORKDIR/order.html" \
  || fail "the web order page does not show the billed status"

grep -q 'Pabellón criollo' "$WORKDIR/order.html" \
  || fail "the web order page does not show the sale line"

grep -q "$DISPLAY_CODE" "$WORKDIR/order.html" \
  || fail "the web order page does not show the display code"

echo "e2e: OK order=$ORDER_ID sale=$SALE_NUMBER total_minor=$TOTAL_MINOR operations=$OPERATIONS"
