#!/usr/bin/env bash
# Idempotent admin-account bootstrap for idol-appreciation.club.
#
#   ./scripts/bootstrap-admin.sh             # create/reuse + promote + verify
#   ./scripts/bootstrap-admin.sh --approve USERNAME
#
# Credentials live ONLY in the k8s secret `admin-account` (namespace idol):
#   kubectl -n idol get secret admin-account -o jsonpath='{.data.username}' | base64 -d
#   kubectl -n idol get secret admin-account -o jsonpath='{.data.password}' | base64 -d
#
# Rotation (v1): the backend has no change-password endpoint yet. Generate a
# new password, update the secret, then update the hash via psql:
#   ARGON2=$(argon2 "$(pwgen)" -id -e 2>/dev/null)  # or the backend's hash_password format
#   psql "$ADMIN_URI" -c "UPDATE accounts SET password_hash='$ARGON2' WHERE username='vreid'"
# (tracked as a backend TODO: change-password endpoint).
set -euo pipefail

NS=idol
USERNAME=vreid
BASE_URL="${BASE_URL:-https://idol-appreciation.club}"
KUBECTL="${KUBECTL:-kubectl}"

if [ "${1:-}" = "--approve" ]; then
  TARGET="${2:?usage: bootstrap-admin.sh --approve USERNAME}"
  echo "approving $TARGET via psql (approved column lands with the approval feature)"
  $KUBECTL -n "$NS" delete pod bootstrap-admin-psql --ignore-not-found >/dev/null
  $KUBECTL -n "$NS" run bootstrap-admin-psql --restart=Never \
    --image=postgres:16-alpine \
    --env="PGURI=$(cd tofu && tofu output -raw pg_admin_uri | sed -E 's|^(postgres://[^/]+/)[^?]+|\1app|')" \
    --env="SQL=UPDATE accounts SET approved = true WHERE username = '$TARGET'" \
    -- sh -c 'psql "$PGURI" -v ON_ERROR_STOP=1 -c "$SQL"' \
    >/dev/null 2>&1 || true
  sleep 10
  $KUBECTL -n "$NS" logs bootstrap-admin-psql 2>/dev/null | tail -2
  $KUBECTL -n "$NS" delete pod bootstrap-admin-psql --force >/dev/null 2>&1 || true
  exit 0
fi

# ---- credentials: reuse or create ----------------------------------------
if $KUBECTL -n "$NS" get secret admin-account >/dev/null 2>&1; then
  USERNAME=$($KUBECTL -n "$NS" get secret admin-account -o jsonpath='{.data.username}' | base64 -d)
  PASSWORD=$($KUBECTL -n "$NS" get secret admin-account -o jsonpath='{.data.password}' | base64 -d)
  echo "reusing existing admin-account secret (username=$USERNAME)"
else
  PASSWORD=$(openssl rand -hex 16)   # 32 chars
  $KUBECTL -n "$NS" create secret generic admin-account \
    --from-literal=username="$USERNAME" \
    --from-literal=password="$PASSWORD" \
    --from-literal=role=admin
  echo "created admin-account secret (username=$USERNAME)"
fi

# ---- register (idempotent: taken == fine) --------------------------------
code=$(curl -s -o /tmp/reg.out -w "%{http_code}" --max-time 20 \
  -H 'Content-Type: application/json' \
  -d "{\"username\":\"$USERNAME\",\"password\":\"$PASSWORD\"}" \
  "$BASE_URL/api/auth/register" || echo 000)
case "$code" in
  200) echo "registered new admin account" ;;
  000) echo "ERROR: cannot reach $BASE_URL" >&2; exit 1 ;;
  *)   echo "register -> $code (expected if account exists)" ;;
esac

# ---- promote via psql (demote everyone else) ------------------------------
ADMIN_URI=$(cd "$(dirname "$0")/../tofu" && tofu output -raw pg_admin_uri)
# the service URI lands on the default maintenance DB; accounts lives in the app DB
ADMIN_URI=$(printf '%s' "$ADMIN_URI" | sed -E 's|^(postgres://[^/]+/)[^?]+|\1app|')
$KUBECTL -n "$NS" delete pod bootstrap-admin-psql --ignore-not-found >/dev/null 2>&1
SQL_PROMOTE="UPDATE accounts SET is_admin = (username = '$USERNAME')"
$KUBECTL -n "$NS" run bootstrap-admin-psql --restart=Never \
  --image=postgres:16-alpine --env="PGURI=$ADMIN_URI" --env="SQL=$SQL_PROMOTE" \
  -- sh -c 'psql "$PGURI" -v ON_ERROR_STOP=1 -c "$SQL"' \
  >/dev/null 2>&1 || true
sleep 10
$KUBECTL -n "$NS" logs bootstrap-admin-psql 2>/dev/null | tail -2
$KUBECTL -n "$NS" delete pod bootstrap-admin-psql --force >/dev/null 2>&1 || true

# ---- verify ----------------------------------------------------------------
jar=$(mktemp)
lcode=$(curl -s -o /dev/null -w "%{http_code}" -c "$jar" --max-time 20 \
  -H 'Content-Type: application/json' \
  -d "{\"username\":\"$USERNAME\",\"password\":\"$PASSWORD\"}" \
  "$BASE_URL/api/auth/login")
me=$(curl -s -b "$jar" --max-time 20 "$BASE_URL/api/auth/me")
rm -f "$jar"
echo "login -> $lcode"
echo "me -> $me"
echo "$me" | jq -e '.is_admin == true' >/dev/null && echo "VERIFIED: $USERNAME is admin" || {
  echo "ERROR: admin verification failed" >&2
  exit 1
}
