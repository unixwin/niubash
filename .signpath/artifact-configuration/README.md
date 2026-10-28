# SignPath artifact configuration (niubash)

Source of truth for what SignPath signs in a niubash release. The
authoritative file is `artifact-configuration.xml` in this directory; this
README is the setup walkthrough and the record of open decisions.

Status: **skeleton — inert until SignPath credentials exist** (application
approved 2026-10-25, test-certificate stage; see
`docs/planning/code-signing-free-options.md` §7 for the progress log).

## How the pieces fit

```
release.yml (build-windows)          produces dist/niubash-v<ver>-win-<arch>/
                                     + setup.exe, zip, tar.gz, 7z
release.yml (signpath-sign)          PENDING CREDENTIALS:
  1. build-signpath-artifact-zip.ps1   normalizes entry names into signpath-input.zip
  2. actions/upload-artifact           SignPath's trusted-build connector only
                                       accepts artifacts stored on GitHub
  3. signpath/github-action-           submits with the CI user token, waits
     submit-signing-request@v3         for approval + signing, downloads the
                                       signed zip, extracts it
  4. apply-signpath-signatures.ps1     writes signed bytes back, re-links the
                                       NTFS hardlink families
  5. repack zip/tar.gz/7z + aliases    uploads *-signpath-signed
     + .sha256 sidecars
release.yml (release job)            merges *-signpath-signed over the plain
                                     artifacts before SHA256SUMS and publishing
```

## One-time setup (after the SignPath org exists)

1. Sign in to `https://app.signpath.io`, create (or open) the **niubash**
   project in the organization.
2. Upload this directory's `artifact-configuration.xml`: Project >
   **Artifact configurations** > add a version (upload the XML file or
   paste it into the live editor; SignPath renders it graphically). Note
   the configuration's slug if you name one.
3. Note the values the workflow needs: **organization ID** (Organization
   settings), the **project slug**, and the **signing policy slug** (for
   the Foundation's test-certificate phase this is the test-signing
   policy the approval email names).
4. Create a **CI user** in the organization with *submitter* permission
   on the project/signing policy, generate its **API token**, and rotate
   it on the schedule the Foundation requires.
5. Add four GitHub Actions secrets to `unixwin/niubash` (Settings >
   Secrets and variables > Actions):

   | Secret | Value |
   | --- | --- |
   | `SIGNPATH_ORG_ID` | SignPath organization ID (GUID) |
   | `SIGNPATH_PROJECT_ID` | the SignPath project **slug** |
   | `SIGNPATH_SIGNING_POLICY_SLUG` | signing policy **slug** |
   | `SIGNPATH_API_TOKEN` | CI user API token |

   The `signpath-gate` job enables `signpath-sign` only when **all four**
   are non-empty. Enabling signing is a repo-settings-only change: no
   code or workflow edit is needed. Likewise, deleting any one secret
   turns the skeleton back off and releases ship unsigned as before.

## Open decisions (resolve during/after the setup review)

1. **Metadata restrictions (Foundation terms requirement).**
   signpath.org/terms requires signed binaries to carry and *enforce*
   version metadata (product name = project name, product version
   consistent per build). `niu.exe`, `winuxcmd.exe` and the shim
   binaries currently embed no VERSIONINFO resource, so the shipped
   configuration declares **no** metadata restrictions yet (they would
   fail every request). Add VERSIONINFO resources first (winres /
   embed-resource in build.rs), then tighten `artifact-configuration.xml`
   with a `pe-file-set` declaration — the placeholder comment in the XML
   marks the spot.
2. **Upstream OSS payloads.** The config signs every PE under
   `winuxcmd\` including `opt\` payloads (gawk, niugit). The Foundation
   terms say upstream OSS binaries must not be signed with the
   subscription — but usr\bin shims are hardlinks of those exact payload
   bytes, so excluding opt\ would split hardlink families. Raise this
   with SignPath Foundation during the setup review; the outcome is
   either "fine (they are your builds)" or a one-line narrowing of the
   `<include>` plus a hardlink-family policy in the write-back script.
3. **Approval latency.** Foundation signing requests require manual
   approver sign-off per release. The workflow waits up to one hour
   (`wait-for-completion-timeout-in-seconds`); the approval link is in
   the step output (`signing-request-web-url`). If approvals routinely
   take longer, raise the timeout or restructure into submit + fetch
   jobs.
4. **Inno Setup uninstaller.** The setup.exe is signed whole, after
   compilation (the simple documented SignPath shape). The uninstaller
   exe Inno embeds at compile time stays unsigned — Windows does not
   check it for reputation. SignPath's (historical) Inno Setup article
   covers compile-time signing via Inno's `SignTool` directive + the
   SignPath PowerShell module if that ever needs revisiting.
5. **Defender scan ordering.** The pre-release Defender scan
   (`scripts/release/defender-scan.ps1`, warn-only) runs in
   build-windows on the *unsigned* bytes. Once signing is live, consider
   re-running (or moving) it against the signed artifacts.

## Local dry run

```powershell
# from a release checkout with a built package:
pwsh scripts/release/build-signpath-artifact-zip.ps1 `
  -PackageDir dist/niubash-v1.4.4-win-x64 `
  -SetupExe  dist/niubash-v1.4.4-win-x64-setup.exe `
  -OutputZip out/signpath-input.zip `
  -MappingJson out/signpath-input-mapping.json
```

The mapping JSON is the write-back contract: every entry records the
original absolute path and its pre-signing SHA-256, which
`apply-signpath-signatures.ps1` uses to put signed bytes back and to
re-link the hardlink families deterministically.
