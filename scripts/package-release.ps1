param(
    [string]$Version,
    [string]$WinuxCmdPath,
    [string]$Configuration = "release",
    [string]$Target,
    [string]$Arch,
    [switch]$AllowPathWinuxCmd
)

$ErrorActionPreference = "Stop"

$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
Push-Location $RepoRoot
try {
    if (-not $Version) {
        $cargoToml = Get-Content -LiteralPath "Cargo.toml" -Raw
        if ($cargoToml -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
            throw "Could not read package version from Cargo.toml"
        }
        $Version = $Matches[1]
    }

    if ($Target) {
        $winuxshExe = Join-Path $RepoRoot "target\$Target\$Configuration\winuxsh.exe"
        $winuxfetchExe = Join-Path $RepoRoot "target\$Target\$Configuration\winuxfetch.exe"
    }
    else {
        $winuxshExe = Join-Path $RepoRoot "target\$Configuration\winuxsh.exe"
        $winuxfetchExe = Join-Path $RepoRoot "target\$Configuration\winuxfetch.exe"
    }
    if (-not (Test-Path -LiteralPath $winuxshExe) -or -not (Test-Path -LiteralPath $winuxfetchExe)) {
        $buildArgs = @("build", "--locked")
        if ($Configuration -eq "release") {
            $buildArgs += "--release"
        }
        if ($Target) {
            $buildArgs += @("--target", $Target)
        }
        cargo @buildArgs
    }
    if (-not (Test-Path -LiteralPath $winuxshExe)) {
        throw "winuxsh.exe not found at $winuxshExe"
    }
    if (-not (Test-Path -LiteralPath $winuxfetchExe)) {
        throw "winuxfetch.exe not found at $winuxfetchExe"
    }

    if (-not $WinuxCmdPath -and $AllowPathWinuxCmd) {
        $fromWhere = (& where.exe winuxcmd.exe 2>$null | Select-Object -First 1)
        if ($fromWhere) {
            $WinuxCmdPath = $fromWhere
        }
    }
    if (-not $WinuxCmdPath -or -not (Test-Path -LiteralPath $WinuxCmdPath)) {
        throw "winuxcmd.exe not found. Pass an explicit -WinuxCmdPath C:\path\to\winuxcmd.exe"
    }

    $activationScript = Join-Path $RepoRoot "assets\winuxcmd\activate-winuxcmd.sh"
    if (-not (Test-Path -LiteralPath $activationScript)) {
        throw "Activation script not found at $activationScript"
    }
    $thirdPartyNotices = Join-Path $RepoRoot "THIRD_PARTY_NOTICES.md"
    if (-not (Test-Path -LiteralPath $thirdPartyNotices)) {
        throw "Third-party notices not found at $thirdPartyNotices"
    }

    $distDir = Join-Path $RepoRoot "dist"
    if ($Arch) {
        $packageName = "winuxsh-v$Version-win-$Arch"
    }
    else {
        $packageName = "winuxsh-v$Version"
    }
    $stageDir = Join-Path $distDir $packageName
    $zipPath = Join-Path $distDir "$packageName.zip"

    Remove-Item -LiteralPath $stageDir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $zipPath -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "winuxcmd") | Out-Null

    Copy-Item -LiteralPath $winuxshExe -Destination (Join-Path $stageDir "winuxsh.exe") -Force
    Copy-Item -LiteralPath $winuxfetchExe -Destination (Join-Path $stageDir "winuxfetch.exe") -Force
    Copy-Item -LiteralPath $WinuxCmdPath -Destination (Join-Path $stageDir "winuxcmd\winuxcmd.exe") -Force
    Copy-Item -LiteralPath $activationScript -Destination (Join-Path $stageDir "winuxcmd\activate-winuxcmd.sh") -Force
    Copy-Item -LiteralPath $thirdPartyNotices -Destination (Join-Path $stageDir "THIRD_PARTY_NOTICES.md") -Force

    Compress-Archive -LiteralPath $stageDir -DestinationPath $zipPath -Force

    $files = Get-ChildItem -LiteralPath $stageDir -Recurse -File
    $size = (Get-Item -LiteralPath $zipPath).Length
    Write-Host "Created $zipPath"
    Write-Host "Files: $($files.Count)"
    Write-Host "Zip size: $([Math]::Round($size / 1MB, 2)) MB"
    Write-Host "Contents:"
    $files | ForEach-Object {
        Write-Host "  $($_.FullName.Substring($stageDir.Length + 1))"
    }
}
finally {
    Pop-Location
}
