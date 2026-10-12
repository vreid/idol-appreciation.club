#!/usr/bin/env bash
# Sync Cloudflare DNS records for idol-appreciation.club to a k8s LoadBalancer.
#
# DNS records are NOT managed by OpenTofu: the LoadBalancer IP only exists
# after the ingress-nginx Service is applied to the cluster (post-apply), and
# baking a moving IP into tofu state makes destroy/recreate cycles fight a
# stale address. This script is idempotent: it creates or updates each record
# in place and always sets proxied=false (DNS ONLY, hard requirement).
#
# Usage: sync-dns.sh <lb-ipv4> [lb-ipv6]
# Env:   CLOUDFLARE_API_TOKEN (required)
#        DNS_ZONE (default: idol-appreciation.club)
#
# A records:      apex (DNS_ZONE) and www.<DNS_ZONE>
# AAAA records:   only when a second argument is supplied.

set -euo pipefail

API="https://api.cloudflare.com/client/v4"
ZONE="${DNS_ZONE:-idol-appreciation.club}"

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <lb-ipv4> [lb-ipv6]" >&2
  exit 2
fi
if [[ -z "${CLOUDFLARE_API_TOKEN:-}" ]]; then
  echo "error: CLOUDFLARE_API_TOKEN is not set" >&2
  exit 2
fi

LB_IPV4="$1"
LB_IPV6="${2:-}"

auth() { curl -sS -H "Authorization: Bearer ${CLOUDFLARE_API_TOKEN}" "$@"; }

zone_id="$(auth "${API}/zones?name=${ZONE}" | jq -r '.result[0].id // empty')"
if [[ -z "${zone_id}" ]]; then
  echo "error: zone ${ZONE} not found (check token permissions)" >&2
  exit 1
fi

# upsert <type> <fqdn> <content>
upsert() {
  local type="$1" name="$2" content="$3" record_id body
  record_id="$(auth "${API}/zones/${zone_id}/dns_records?type=${type}&name=${name}" \
    | jq -r '.result[0].id // empty')"
  body="$(jq -n --arg t "${type}" --arg n "${name}" --arg c "${content}" \
    '{type: $t, name: $n, content: $c, ttl: 300, proxied: false}')"

  if [[ -n "${record_id}" ]]; then
    auth -X PUT "${API}/zones/${zone_id}/dns_records/${record_id}" \
      -H "Content-Type: application/json" -d "${body}" > /dev/null
    echo "updated ${type} ${name} -> ${content}"
  else
    auth -X POST "${API}/zones/${zone_id}/dns_records" \
      -H "Content-Type: application/json" -d "${body}" > /dev/null
    echo "created ${type} ${name} -> ${content}"
  fi
}

for name in "${ZONE}" "www.${ZONE}"; do
  upsert A "${name}" "${LB_IPV4}"
done

if [[ -n "${LB_IPV6}" ]]; then
  for name in "${ZONE}" "www.${ZONE}"; do
    upsert AAAA "${name}" "${LB_IPV6}"
  done
fi

# Report the proxied state so callers (and humans) can verify DNS-only.
auth "${API}/zones/${zone_id}/dns_records?per_page=100" \
  | jq -r '.result[] | select(.type=="A" or .type=="AAAA")
           | "\(.type) \(.name) -> \(.content) proxied=\(.proxied)"'
