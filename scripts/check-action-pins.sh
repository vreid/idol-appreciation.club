#!/usr/bin/env bash
# Fails on any third-party `uses:` that is not pinned to a full commit SHA.
#
# Why: mutable refs (tags like @v4) execute whatever the action's repo
# moves the tag to — in this repo that includes jobs holding production
# cloud tokens and cluster credentials (issue #86). Allowed forms:
#   - local composites:            ./.github/actions/...
#   - docker-backed steps:         docker://image[:tag]
#   - everything else:             owner/repo@<40-hex-sha>  (optional comment)
set -euo pipefail
cd "$(dirname "$0")/.."

status=0
while IFS=$'\t' read -r file line ref; do
    case "$ref" in
        ./* | docker://*) continue ;;
        *@*)
            sha="${ref##*@}"
            if [[ ! "$sha" =~ ^[0-9a-f]{40}$ ]]; then
                echo "::error file=$file,line=$line::unpinned action ref '$ref' — pin to a full commit SHA (see scripts/check-action-pins.sh)" >&2
                status=1
            fi
            ;;
        *)
            echo "::error file=$file,line=$line::malformed 'uses:' value '$ref'" >&2
            status=1
            ;;
    esac
done < <(awk '
    /^#/ { next }
    match($0, /uses:/) {
        tail = substr($0, RSTART + RLENGTH)
        sub(/^[[:space:]]+/, "", tail)
        sub(/[[:space:]]+#.*$/, "", tail)
        sub(/[[:space:]]+$/, "", tail)
        path = FILENAME
        lineno = FNR
        # Strip the "- " list-item prefix that may precede uses:.
        print path "\t" lineno "\t" tail
    }
' .github/workflows/*.yml .github/actions/*/action.yml)

if [[ "$status" -eq 0 ]]; then
    echo "action pins: OK (all third-party uses: refs are commit SHAs)"
fi
exit "$status"
