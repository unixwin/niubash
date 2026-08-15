param(
    [string]$Version,
    [string]$WinuxCmdPath,
    [string]$Configuration = "release",
    [string]$Target,
    [string]$Arch,
    [string]$OhMyWinuxshBundlePath,
    [switch]$SkipOhMyWinuxshBundle,
    [switch]$AllowPathWinuxCmd
)

$ErrorActionPreference = "Stop"

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
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
    }
    else {
        $winuxshExe = Join-Path $RepoRoot "target\$Configuration\winuxsh.exe"
    }
    if (-not (Test-Path -LiteralPath $winuxshExe)) {
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
    $iconFiles = @(
        Join-Path $RepoRoot "assets\winuxsh-icon.ico"
        Join-Path $RepoRoot "assets\winuxsh-icon-256.png"
        Join-Path $RepoRoot "assets\winuxsh-icon-64.png"
        Join-Path $RepoRoot "assets\winuxsh-icon.png"
        Join-Path $RepoRoot "assets\winuxsh-icon.svg"
    )
    foreach ($iconFile in $iconFiles) {
        if (-not (Test-Path -LiteralPath $iconFile)) {
            throw "Icon asset not found at $iconFile"
        }
    }

    $resolvedOhMyWinuxshBundlePath = $null
    if (-not $SkipOhMyWinuxshBundle) {
        if ($OhMyWinuxshBundlePath) {
            $bundleCandidates = @($OhMyWinuxshBundlePath)
        }
        else {
            $bundleCandidates = @(
                (Join-Path $RepoRoot "..\oh-my-winuxsh")
                (Join-Path $RepoRoot "bundles\oh-my-winuxsh")
                (Join-Path $RepoRoot "vendor\oh-my-winuxsh")
            )
        }

        foreach ($candidate in $bundleCandidates) {
            if ((Test-Path -LiteralPath $candidate) -and (Test-Path -LiteralPath (Join-Path $candidate "bundle.toml"))) {
                $resolvedOhMyWinuxshBundlePath = (Resolve-Path -LiteralPath $candidate).Path
                break
            }
        }

        if (-not $resolvedOhMyWinuxshBundlePath) {
            throw "oh-my-winuxsh bundle not found. Pass -OhMyWinuxshBundlePath C:\path\to\oh-my-winuxsh or -SkipOhMyWinuxshBundle."
        }
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
    New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "winuxcmd\usr\bin") | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "assets") | Out-Null

    Copy-Item -LiteralPath $winuxshExe -Destination (Join-Path $stageDir "winuxsh.exe") -Force
    Copy-Item -LiteralPath $WinuxCmdPath -Destination (Join-Path $stageDir "winuxcmd\usr\bin\winuxcmd.exe") -Force
    Copy-Item -LiteralPath $activationScript -Destination (Join-Path $stageDir "winuxcmd\usr\bin\activate-winuxcmd.sh") -Force
    foreach ($iconFile in $iconFiles) {
        Copy-Item -LiteralPath $iconFile -Destination (Join-Path $stageDir "assets") -Force
    }
    if ($resolvedOhMyWinuxshBundlePath) {
        $bundleStageDir = Join-Path $stageDir "bundles\oh-my-winuxsh"
        New-Item -ItemType Directory -Force -Path $bundleStageDir | Out-Null
        $requiredBundleEntries = @(
            "oh-my-winuxsh.winux"
            "bundle.toml"
            "index.toml"
            "lib"
            "plugins"
            "packs"
            "themes"
        )
        $bundleEntries = @(
            "oh-my-winuxsh.winux"
            "bundle.toml"
            "index.toml"
            "README.md"
            "CHANGELOG.md"
            "lib"
            "plugins"
            "packs"
            "aliases"
            "completions"
            "prompts"
            "keybindings"
            "themes"
            "wasm"
            "docs"
            "templates"
            "tools"
        )
        foreach ($entry in $bundleEntries) {
            $source = Join-Path $resolvedOhMyWinuxshBundlePath $entry
            if (Test-Path -LiteralPath $source) {
                Copy-Item -LiteralPath $source -Destination $bundleStageDir -Recurse -Force
            }
            elseif ($requiredBundleEntries -contains $entry) {
                throw "Required oh-my-winuxsh bundle entry missing: $source"
            }
        }
        Get-ChildItem -LiteralPath $bundleStageDir -Recurse -Directory -Filter "__pycache__" |
            Remove-Item -Recurse -Force
        Get-ChildItem -LiteralPath $bundleStageDir -Recurse -File -Include "*.pyc", "*.pyo" |
            Remove-Item -Force
    }

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
