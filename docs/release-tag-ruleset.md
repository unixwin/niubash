# Release tag immutability ruleset

`unixwin/niubash` protects every `v*` git tag with the repository ruleset
`release-tags-are-immutable` (niubash#425, rubash#827 recommendation 4).
It is a **tag ruleset**, enforced by GitHub on the server side — not a
hook, not a convention.

## Why

rubash#827's forensics recorded the niubash `v1.4.0` tag being re-pushed
at least six times in one release cycle (heads `b9799a40` / `7e399890` /
`285d87f9` / `c7b9ae2b` / `7c9a10d1` — all red → `03af1a65` green →
`20ec3ef0` final) to wait out a green pipeline. Each re-push fired a
fresh `push: tags: v*` release run against a moving commit while the tag
name was the ecosystem's version pointer. The ruleset closes that hole:
a pushed `v*` tag is final release state.

## Configuration (applied)

| Field | Value |
| --- | --- |
| Ruleset name | `release-tags-are-immutable` |
| Target | `tag` |
| Enforcement | `active` |
| Ref pattern | include `refs/tags/v*` |
| Restrict deletions | on (`deletion` rule) |
| Restrict updates | on (`update` rule) — force-push/re-push rejected |
| Bypass actors | `OrganizationAdmin`, `always` |

Effects once active:

- Anyone with `write`/`maintain` can **create** a new `v*` tag — cutting
  a release stays normal — but cannot move or delete an existing one.
- A bad release is corrected **forward** with a new tag
  (`vX.Y.(Z+1)`-style), never by rewriting the old one — the same
  fix-forward rule rubash#827 applied to already-shipped assets.
- A failed release run on a tag that is itself correct is re-flown with
  `workflow_dispatch` against that tag (the workflow accepts `tag`,
  `winuxcmd_tag`, `winuxcmd_sha256` inputs and publishes through the same
  `release` job).
- The ruleset guards the **git ref only**. Release-page prose and asset
  uploads stay editable — normal release maintenance, out of scope.

Why a ruleset and not the legacy `Settings → Tags` protected-tag
patterns: the legacy feature only filters *who* may create/update a
matching tag and lets everyone on its allow-list force-update. A
ruleset's `update` restriction rejects the update for everyone except a
listed bypass actor, and the ruleset itself is auditable over the API.

## Apply / inspect

The exact payload is `scripts/release/tag-ruleset.json`, applied by
`scripts/release/apply-tag-ruleset.sh` (idempotent: PUTs over an existing
same-named tag ruleset, POSTs otherwise). Requires `gh` authenticated as
an org admin — repository rulesets need Administration write.

```sh
scripts/release/apply-tag-ruleset.sh
NIUBASH_REPO=owner/repo scripts/release/apply-tag-ruleset.sh   # other repos

# inspect
gh api repos/unixwin/niubash/rulesets --jq '.[] | {id, name, target, enforcement}'
gh api repos/unixwin/niubash/rulesets/<id>
```

Changing the protection is a PR against `tag-ruleset.json` plus one run
of the script — the API object is the reviewable artifact.

## Operator impact

- **A tag push is one-shot.** Confirm `origin/master` is final before
  pushing `vX.Y.Z`; there is no delete-and-re-push without an admin
  bypass. The release runbook's ignition sequence was updated for this
  (docs/release-runbook.md).
- **Re-fly, don't re-push.** For a failed run on a correct tag, use
  `gh workflow run release.yml -f tag=vX.Y.Z -f winuxcmd_tag=... -f
  winuxcmd_sha256=...` instead of moving the tag.
- **Emergencies.** Org admins bypass always (`current_user_can_bypass:
  always`); everyone else is hard-blocked. The bypass exists for genuine
  emergencies, not as the re-trigger path.

## Not covered here

- The `v1.3.4` ghost version (`485a12cd` — tag exists, release run
  37218948432 failed, no release page ever published). niubash#425 leaves
  "publish a tombstone page" vs "record as never-released" to the owner;
  neither option touches the tag ref.
