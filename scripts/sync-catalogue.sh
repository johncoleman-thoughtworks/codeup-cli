#!/usr/bin/env bash
# Pull the latest default.yaml catalogue from codeup-vscx (the canonical
# source) into this repo's embedded copy. Run whenever upstream bumps the
# catalogue or before cutting a release if you want to ship the latest set.
#
# Usage:
#   scripts/sync-catalogue.sh           # default branch (main)
#   scripts/sync-catalogue.sh v1.2.3    # specific tag / commit / branch
set -euo pipefail

REF="${1:-main}"
URL="https://raw.githubusercontent.com/johncoleman-thoughtworks/codeup-vscx/${REF}/resources/catalogue/default.yaml"
DEST_DIR="crates/codeup-core/resources"
DEST="${DEST_DIR}/default.yaml"

mkdir -p "${DEST_DIR}"

echo "Fetching catalogue from ${URL}"
TMP="$(mktemp)"
trap 'rm -f "${TMP}"' EXIT
curl -fsSL "${URL}" -o "${TMP}"

# Cheap sanity check — must look like our catalogue
if ! head -5 "${TMP}" | grep -q "schemaVersion"; then
  echo "ERROR: fetched file does not look like a catalogue (missing schemaVersion)" >&2
  exit 1
fi

mv "${TMP}" "${DEST}"
trap - EXIT
COUNT=$(grep -c '^  - id:' "${DEST}" || true)
BYTES=$(wc -c < "${DEST}" | tr -d ' ')
echo "Wrote ${DEST} (${COUNT} patterns, ${BYTES} bytes, ref=${REF})"
