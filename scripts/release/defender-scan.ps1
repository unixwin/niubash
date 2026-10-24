# niubash#511 (section 4.4): pre-release Defender gate for dist artifacts.
#
# Scans the given files (or every *.exe / *.msix under the given
# directories) with the locally installed Microsoft Defender engine and
# fails closed: exit code 1 when any artifact is convicted or any scan
# errors, so a release pipeline step can block publication instead of
# shipping bytes that Defender will quarantine on user machines (the
# 1.4.4 incident: Behavior:Win32/DefenseEvasion.A!ml on niu.exe).
#
# Honest limits, from the #511 audit: this catches signature-level and
# static-ML convictions only. Hosted CI runners ship with real-time
# protection off and tamper protection off, so cloud/behavioral verdicts
# may NOT reproduce there; a green run here is necessary, not sufficient.
# When this gate goes red, the first SOP step is a WDSI false-positive
# submission with the exact artifact bytes (see niubash#511 section 4.1);
# a human override may still ship after that submission is filed.
#
# Usage:
#   pwsh scripts/release/defender-scan.ps1 dist\niubash-v1.4.5-win-x64-setup.exe
#   pwsh scripts/release/defender-scan.ps1 dist\
#
# Not wired into .github/workflows/release.yml yet: adopting it as a
# blocking release step (and the override policy) is a maintainer call.
param(
    [Parameter(Mandatory = $true, Position = 0, ValueFromRemainingArguments = $true)]
    [string[]]$Path,
    # Skip the signature refresh. CI images should NOT use this: stale
    # definitions judge artifacts against the wrong rulebook.
    [switch]$SkipSignatureUpdate
)

$ErrorActionPreference = "Stop"

function Find-MpCmdRun {
    # Platform installs live under a versioned directory; the legacy
    # in-box path is the fallback. Pick the newest platform copy.
    $platformRoot = Join-Path $env:ProgramData "Microsoft\Windows Defender\Platform"
    if (Test-Path -LiteralPath $platformRoot) {
        $candidate = Get-ChildItem -LiteralPath $platformRoot -Directory |
            Sort-Object Name -Descending |
            ForEach-Object { Join-Path $_.FullName "MpCmdRun.exe" } |
            Where-Object { Test-Path -LiteralPath $_ } |
            Select-Object -First 1
        if ($candidate) { return $candidate }
    }
    $legacy = Join-Path $env:ProgramFiles "Windows Defender\MpCmdRun.exe"
    if (Test-Path -LiteralPath $legacy) { return $legacy }
    throw "MpCmdRun.exe not found (looked under $platformRoot and $legacy); is Defender installed on this machine?"
}

$mpCmdRun = Find-MpCmdRun
Write-Host "Defender engine: $mpCmdRun"

if (-not $SkipSignatureUpdate) {
    # runner-images#855: hosted Windows images need wuauserv startable
    # before -SignatureUpdate can pull fresh definitions.
    try {
        Set-Service -Name wuauserv -StartupType Manual -ErrorAction Stop
        Start-Service -Name wuauserv -ErrorAction Stop
    } catch {
        Write-Host "::warning::could not start wuauserv ($($_.Exception.Message)); signature update may fail"
    }
    & $mpCmdRun -SignatureUpdate | Out-Host
    if ($LASTEXITCODE -ne 0) {
        throw "MpCmdRun -SignatureUpdate failed with exit code $LASTEXITCODE"
    }
}

# Anchor the verdict to a definitions version, per the #511 proposal to
# record signature state with the release evidence.
try {
    $status = Get-MpComputerStatus
    Write-Host "AMProductVersion: $($status.AMProductVersion)"
    Write-Host "AntivirusSignatureVersion: $($status.AntivirusSignatureVersion)"
    Write-Host "AntivirusSignatureLastUpdated: $($status.AntivirusSignatureLastUpdated)"
} catch {
    Write-Host "::warning::Get-MpComputerStatus unavailable ($($_.Exception.Message)); continuing without the signature anchor"
}

$files = foreach ($item in $Path) {
    $resolved = Resolve-Path -LiteralPath $item
    foreach ($entry in $resolved) {
        if (Test-Path -LiteralPath $entry.Path -PathType Container) {
            Get-ChildItem -LiteralPath $entry.Path -Recurse -File |
                Where-Object { $_.Extension -in ".exe", ".msix" }
        } else {
            Get-Item -LiteralPath $entry.Path
        }
    }
}
if (-not $files) {
    throw "no scannable artifacts (*.exe / *.msix) found under: $($Path -join ', ')"
}

$detections = @()
$errors = @()
foreach ($file in $files) {
    Write-Host "scanning $($file.FullName)"
    # -ScanType 3 = custom scan of the given file; -DisableRemediation so
    # the gate observes the verdict without quarantining build outputs.
    & $mpCmdRun -Scan -ScanType 3 -File $file.FullName -DisableRemediation | Out-Host
    switch ($LASTEXITCODE) {
        0 { }
        # MpCmdRun exit code 2 = threat found / remediation required.
        2 { $detections += $file.FullName }
        default { $errors += "$($file.FullName) (MpCmdRun exit code $LASTEXITCODE)" }
    }
}

if ($detections) {
    Write-Host "::error::Defender convicted $($detections.Count) artifact(s):"
    $detections | ForEach-Object { Write-Host "  convicted: $_" }
    Write-Host "Next step per niubash#511 section 4.1: file a WDSI false-positive submission with these exact bytes before any override."
}
if ($errors) {
    Write-Host "::error::Defender scan errored on $($errors.Count) artifact(s):"
    $errors | ForEach-Object { Write-Host "  error: $_" }
}
if ($detections -or $errors) {
    exit 1
}
Write-Host "Defender gate: $($files.Count) artifact(s) clean."
