# Build the SignPath input zip for a completed niubash Windows release layout.
#
# SignPath's GitHub trusted-build flow (docs.signpath.io/trusted-build-
# systems/github) signs a zip whose entry names must match the project's
# artifact configuration (.signpath/artifact-configuration/artifact-
# configuration.xml). This script turns a real release layout — the
# extracted portable package tree plus the Inno Setup installer — into that
# normalized zip:
#
#     setup.exe                 the *-setup.exe installer (renamed, stable slot)
#     niu.exe                   the shell
#     winuxcmd/...              every PE (.exe/.dll) of the package tree,
#                               at its package-relative path
#
# Stable entry names are the whole point: the artifact configuration
# declares paths once and never needs a version- or arch-specific update.
#
# The sidecar mapping JSON is the write-back contract: one record per zip
# entry with the original absolute path and the PRE-signing SHA-256.
# scripts/release/apply-signpath-signatures.ps1 consumes it to put the
# signed bytes back and to re-link the NTFS hardlink families (wpm's shim
# layout is hardlink-based; the package zip flattens links into
# byte-identical independent copies, and the identical pre-signing hash is
# what identifies a family).
#
# Runs on Windows and Linux (pwsh 7+): the release workflow uses it on an
# ubuntu runner, the local dry run is Windows.

param(
    # The extracted portable package tree, i.e. the directory the release
    # zip contains: <PackageDir>/niu.exe, <PackageDir>/winuxcmd/...
    [Parameter(Mandatory = $true)]
    [string]$PackageDir,

    # The versioned Inno Setup installer, e.g.
    # dist/niubash-v1.4.4-win-x64-setup.exe.
    [Parameter(Mandatory = $true)]
    [string]$SetupExe,

    # Where to write the SignPath input zip.
    [Parameter(Mandatory = $true)]
    [string]$OutputZip,

    # Where to write the mapping JSON (write-back contract, see above).
    [Parameter(Mandatory = $true)]
    [string]$MappingJson
)

$ErrorActionPreference = "Stop"

function Get-Sha256([string]$Path) {
    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

$PackageDir = (Resolve-Path -LiteralPath $PackageDir).Path
if (-not (Test-Path -LiteralPath $PackageDir -PathType Container)) {
    throw "PackageDir is not a directory: $PackageDir"
}

# Sanity: this must be a niubash Windows package tree, not some adjacent
# directory. Both markers are structural (niubash#189 layout).
foreach ($marker in @("niu.exe", "winuxcmd\usr\bin\winuxcmd.exe")) {
    if (-not (Test-Path -LiteralPath (Join-Path $PackageDir $marker))) {
        throw "not a niubash package tree (missing $marker): $PackageDir"
    }
}

$SetupExe = (Resolve-Path -LiteralPath $SetupExe).Path
if ($SetupExe -notmatch '-setup\.exe$') {
    throw "SetupExe does not look like an Inno Setup installer: $SetupExe"
}

foreach ($out in @($OutputZip, $MappingJson)) {
    $parent = Split-Path -Parent $out
    if ($parent) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
    if (Test-Path -LiteralPath $out) { Remove-Item -LiteralPath $out -Force }
}

# Collect every PE of the package tree at its package-relative path. The
# archive layout deliberately mirrors the package, so the write-back is a
# pure per-entry copy and the artifact configuration can match the whole
# tree with the stable winuxcmd/** wildcards.
$peEntries =
    Get-ChildItem -LiteralPath $PackageDir -Recurse -File |
    Where-Object { $_.Extension -in @(".exe", ".dll") } |
    ForEach-Object {
        $relative = $_.FullName.Substring($PackageDir.Length + 1).Replace("\", "/")
        [pscustomobject]@{
            entry  = $relative
            source = $_.FullName
        }
    } |
    Sort-Object { $_.entry }

if (-not $peEntries) {
    throw "no .exe/.dll files found under $PackageDir; refusing to build an empty SignPath artifact"
}
if ($peEntries.entry -contains "setup.exe") {
    throw "the package tree contains a top-level setup.exe; the installer slot must stay unambiguous"
}

# The installer gets the stable top-level slot.
$allEntries = @(
    [pscustomobject]@{ entry = "setup.exe"; source = $SetupExe }
) + $peEntries

# Late assertions on the shape the artifact configuration depends on:
# min-matches for winuxcmd/**/*.exe is the default 1, so the tree must
# genuinely carry at least the dispatcher, and niu.exe is declared
# individually.
foreach ($required in @("niu.exe", "winuxcmd/usr/bin/winuxcmd.exe")) {
    if ($allEntries.entry -notcontains $required) {
        throw "required PE '$required' is not part of the SignPath artifact; the artifact configuration declares it"
    }
}

Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

$zip = [System.IO.Compression.ZipFile]::Open($OutputZip, [System.IO.Compression.ZipArchiveMode]::Create)
try {
    foreach ($item in $allEntries) {
        $null = [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
            $zip, $item.source, $item.entry,
            [System.IO.Compression.CompressionLevel]::Optimal)
    }
}
finally {
    $zip.Dispose()
}

# The write-back contract. Hashes are taken AFTER the zip write so a
# mid-read mutation of the sources cannot go unnoticed (the zip and the
# mapping then describe different bytes).
$mapping = foreach ($item in $allEntries) {
    [pscustomobject]@{
        entry     = $item.entry
        source    = $item.source
        sha256    = Get-Sha256 $item.source
        length    = (Get-Item -LiteralPath $item.source).Length
    }
}
$mapping | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $MappingJson -Encoding utf8

# Self-check: the zip must carry exactly the mapped entries.
$zipVerify = [System.IO.Compression.ZipFile]::OpenRead($OutputZip)
try {
    $zipEntryCount = $zipVerify.Entries.Count
    $zipNames = $zipVerify.Entries.FullName | Sort-Object
}
finally {
    $zipVerify.Dispose()
}
if ($zipEntryCount -ne $allEntries.Count) {
    throw "input zip holds $zipEntryCount entries, expected $($allEntries.Count)"
}
if (Compare-Object -ReferenceObject ($allEntries.entry | Sort-Object) -DifferenceObject $zipNames) {
    throw "input zip entry names diverge from the mapping"
}

$size = (Get-Item -LiteralPath $OutputZip).Length
Write-Host "SignPath input zip: $OutputZip ($([Math]::Round($size / 1MB, 2)) MB, $zipEntryCount entries)"
Write-Host "Mapping JSON:       $MappingJson"
Write-Host "  setup.exe <- $(Split-Path -Leaf $SetupExe)"
Write-Host "  $($peEntries.Count) package PEs (exe+dll) under their package-relative names"
