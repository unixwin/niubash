#!/usr/bin/env bash
# Applies the release-tag immutability ruleset (niubash#425, rubash#827
# recommendation 4) to a repository through the GitHub rulesets REST API.
#
# The payload lives in tag-ruleset.json next to this script so the exact
# protection is reviewable in git. The script is idempotent: when a tag
# ruleset with the same name already exists on the repository it is
# updated (PUT), otherwise it is created (POST).
#
# Usage:
#   scripts/release/apply-tag-ruleset.sh                  # unixwin/niubash
#   NIUBASH_REPO=owner/repo scripts/release/apply-tag-ruleset.sh
#
# Requires: gh authenticated with an admin-capable token (repository
# rulesets need Administration write; check `gh auth status`).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PAYLOAD="$SCRIPT_DIR/tag-ruleset.json"
REPO="${NIUBASH_REPO:-unixwin/niubash}"

command -v gh >/dev/null 2>&1 || { echo "error: gh CLI not found on PATH" >&2; exit 1; }

ruleset_id="$(
  gh api "repos/$REPO/rulesets" \
    --jq '.[] | select(.target == "tag" and .name == "release-tags-are-immutable") | .id'
)"

if [ -n "$ruleset_id" ]; then
  echo "updating existing tag ruleset $ruleset_id on $REPO"
  gh api "repos/$REPO/rulesets/$ruleset_id" --method PUT --input "$PAYLOAD" >/dev/null
else
  echo "creating tag ruleset on $REPO"
  ruleset_id="$(gh api "repos/$REPO/rulesets" --method POST --input "$PAYLOAD" --jq .id)"
fi

# Report the effective state, not just the request: enforcement and the
# caller's bypass verdict are what the protection means in practice.
gh api "repos/$REPO/rulesets/$ruleset_id" \
  --jq '{id, name, target, enforcement, rules: [.rules[].type], bypass_actors, current_user_can_bypass}'
