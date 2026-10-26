# Apply SignPath-signed bytes back into a niubash release layout.
#
# Counterpart to build-signpath-artifact-zip.ps1: consumes the SAME mapping
# JSON (the write-back contract) plus the directory SignPath's GitHub action
# extracted the signed artifact into (output-artifact-directory; the signed
# zip carries the same normalized entry names) and
#
#   1. copies every signed entry over its original source path,
#   2. restores the NTFS hardlink families the package layout depends on.
#
# Family restoration is the subtle half. The package tree ships wpm's
# hardlink layout (usr\bin shims are the SAME BYTES as their opt\ payloads,
# bash.exe/sh.exe are the same shim bytes, the niugit git.exe family is one
# binary), but the zip the package traveled in flattens links into
# byte-identical independent copies, and SignPath signs each zip entry
# independently — so family members can come back with different signature
# timestamps, i.e. different bytes. The pre-signing SHA-256 in the mapping
# identifies each family; this script makes every member byte-identical to
# the family's lexicographically-first entry (loud warning when SignPath
# returned divergent bytes — the warning is informative, normalization is
# required) and re-links them as hardlinks. Content-identical faces are the
# shipped contract (package-release.ps1 stages the same shim bytes under
# bash.exe/sh.exe/niu-shim.exe; face dispatch is argv[0]-based), so
# normalization cannot change behavior — it restores it.
#
# Runs on Windows and Linux (pwsh 7+). Hardlink re-creation uses
# New-Item -ItemType HardLink (supported on both platforms in pwsh 7.1+)
# with an `ln -f` fallback.

param(
    # The extracted portable package tree the input zip was built from —
    # its files are replaced with the signed bytes.
    [Parameter(Mandatory = $true)]
    [string]$PackageDir,

    # The versioned Inno Setup installer to replace with its signed bytes.
    [Parameter(Mandatory = $true)]
    [string]$SetupExe,

    # Directory holding the SIGNED artifact's extracted entries (the
    # GitHub action's output-artifact-directory with skip-decompress at
    # its default).
    [Parameter(Mandatory = $true)]
    [string]$SignedDir,

    # The mapping JSON written by build-signpath-artifact-zip.ps1.
    [Parameter(Mandatory = $true)]
    [string]$MappingJson
)

$ErrorActionPreference = "Stop"

$PackageDir = (Resolve-Path -LiteralPath $PackageDir).Path
$SignedDir = (Resolve-Path -LiteralPath $SignedDir).Path
if (-not (Test-Path -LiteralPath $MappingJson)) {
    throw "mapping JSON not found: $MappingJson"
}
$mapping = Get-Content -LiteralPath $MappingJson -Raw | ConvertFrom-Json
if (-not $mapping -or -not $mapping.entry) {
    throw "mapping JSON carries no entries: $MappingJson"
}

function Get-Sha256Bytes([string]$Path) {
    # Byte hash as a comparable string, without re-hexing per comparison.
    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
}

function Assert-SignedPe([string]$Path, [object]$expected) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "signed artifact missing entry '$($expected.entry)' (looked at $Path)"
    }
    # Authenticode signing appends to (never truncates) the image, so the
    # signed file cannot be smaller than what was submitted.
    $length = (Get-Item -LiteralPath $Path).Length
    if ($length -lt $expected.length) {
        throw ("signed '{0}' shrank ({1} < {2} bytes); refusing to apply" -f `
            $expected.entry, $length, $expected.length)
    }
    $fs = [System.IO.File]::OpenRead($Path)
    try {
        $mz = [byte[]]::new(2)
        $null = $fs.Read($mz, 0, 2)
        if ($mz[0] -ne 0x4D -or $mz[1] -ne 0x5A) {
            throw "signed entry '$($expected.entry)' is not an MZ image"
        }
    }
    finally {
        $fs.Dispose()
    }
}

# 1. Copy every signed entry over its original location.
foreach ($item in $mapping) {
    $signedPath = Join-Path $SignedDir ($item.entry -replace "/", [System.IO.Path]::DirectorySeparatorChar)
    Assert-SignedPe $signedPath $item
    [System.IO.File]::Copy($signedPath, $item.source, $true)
    Write-Host ("signed <- {0}" -f $item.entry)
}

# 2. Restore the hardlink families. Grouping key: the PRE-signing SHA-256
# (family members were byte-identical before signing — that is the wpm
# layout and what the package zip flattened).
$families = @{}
foreach ($item in $mapping) {
    if (-not $families.ContainsKey($item.sha256)) {
        $families[$item.sha256] = [System.Collections.Generic.List[object]]::new()
    }
    $families[$item.sha256].Add($item)
}

function Copy-Bytes([string]$From, [string]$To) {
    [System.IO.File]::Copy($From, $To, $true)
}

function Join-HardLink([string]$Link, [string]$Target) {
    try {
        New-Item -ItemType HardLink -Path $Link -Target $Target -Force -ErrorAction Stop | Out-Null
        return $true
    }
    catch {
        # Fallback for hosts where the cmdlet lacks Unix support.
        & ln -f -- $Target $Link 2>$null
        return ($LASTEXITCODE -eq 0)
    }
}

$familyCount = 0
$linkCount = 0
$normalized = 0
foreach ($group in $families.Values) {
    if ($group.Count -lt 2) { continue }
    $ordered = $group | Sort-Object { $_.entry }
    $primary = $ordered[0]
    $familyCount++

    # SignPath signed each entry independently: divergence is expected
    # (signature timestamps), consistency is required (one family, one
    # binary). Normalize to the primary, loudly.
    $primaryHash = Get-Sha256Bytes $primary.source
    foreach ($member in ($ordered | Select-Object -Skip 1)) {
        if ((Get-Sha256Bytes $member.source) -ne $primaryHash) {
            Write-Host ("::warning::family '{0}': signed bytes diverged from '{1}' (independent signature timestamps); normalizing the family to the primary's bytes" -f `
                $primary.entry, $member.entry)
            Copy-Bytes $primary.source $member.source
            $normalized++
        }
    }
    foreach ($member in ($ordered | Select-Object -Skip 1)) {
        if (Join-HardLink $member.source $primary.source) {
            $linkCount++
        }
        else {
            # Same degrade path as package-release.ps1's staging sweep: an
            # FS without links keeps independent byte-identical copies.
            Write-Warning "hardlink for '$($member.source)' failed; keeping an independent byte-identical copy"
        }
    }
}

# 3. Final sweep: every mapped source must now be an existing, non-empty PE.
foreach ($item in $mapping) {
    if (-not (Test-Path -LiteralPath $item.source -PathType Leaf)) {
        throw "post-write-back missing file: $($item.source)"
    }
    if ((Get-Item -LiteralPath $item.source).Length -eq 0) {
        throw "post-write-back empty file: $($item.source)"
    }
}

Write-Host ("SignPath write-back complete: {0} PEs, {1} families re-linked ({2} links, {3} members normalized)" -f `
    $mapping.Count, $familyCount, $linkCount, $normalized)
