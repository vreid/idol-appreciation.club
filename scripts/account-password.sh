#!/usr/bin/env bash
# Print the credentials stored in an idol-namespace account secret.
#
#   ./scripts/account-password.sh                  # admin-account (default)
#   ./scripts/account-password.sh seed-account     # the seed holder
#
# The password is the random value generated at secret creation; there is no
# change-password endpoint yet (initial == current). Output goes to stdout —
# pipe it somewhere private, do not paste it into shared logs.
set -euo pipefail

NS=idol
KUBECTL="${KUBECTL:-kubectl}"

usage() {
  echo "usage: $0 [secret-name]" >&2
  echo "  secret-name: admin-account (default) | seed-account" >&2
  exit "${1:-0}"
}

case "${1:---help}" in
  -h|--help|help) usage 0 ;;
  admin-account|seed-account) SECRET="$1" ;;
  *) echo "ERROR: unknown secret '$1' (valid: admin-account, seed-account)" >&2; usage 2 ;;
esac

export KUBECONFIG="${KUBECONFIG:-$HOME/idol-kube/config}"

$KUBECTL version --client >/dev/null 2>&1 || {
  echo "ERROR: kubectl not available" >&2
  exit 1
}
$KUBECTL -n "$NS" get secret "$SECRET" >/dev/null 2>&1 || {
  echo "ERROR: secret '$SECRET' not found in namespace '$NS' (kubeconfig: ${KUBECONFIG})" >&2
  exit 1
}

USER_B64=$($KUBECTL -n "$NS" get secret "$SECRET" -o jsonpath='{.data.username}')
PASS_B64=$($KUBECTL -n "$NS" get secret "$SECRET" -o jsonpath='{.data.password}')
[ -n "$PASS_B64" ] || {
  echo "ERROR: secret '$SECRET' has no .data.password field" >&2
  exit 1
}

echo "secret:   $SECRET (namespace $NS)"
echo "username: $(printf '%s' "$USER_B64" | base64 -d)"
echo "password: $(printf '%s' "$PASS_B64" | base64 -d)"
