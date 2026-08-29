param(
    [string]$Version,
    [string]$WinuxCmdPath,
    [string]$Configuration = "release",
    [string]$Target,
    [string]$Arch,
    [string]$BashShimPath,
    [string]$ShShimPath,
    [string]$OhMyNiubashBundlePath,
    [switch]$SkipOhMyNiubashBundle,
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
        $niubashExe = Join-Path $RepoRoot "target\$Target\$Configuration\niu.exe"
    }
    else {
        $niubashExe = Join-Path $RepoRoot "target\$Configuration\niu.exe"
    }
    if (-not (Test-Path -LiteralPath $niubashExe)) {
        $buildArgs = @("build", "--locked")
        if ($Configuration -eq "release") {
            $buildArgs += "--release"
        }
        if ($Target) {
            $buildArgs += @("--target", $Target)
        }
        cargo @buildArgs
    }
    if (-not (Test-Path -LiteralPath $niubashExe)) {
        throw "niu.exe not found at $niubashExe"
    }

    function Resolve-RubashShim {
        param(
            [string]$Name,
            [string]$ExplicitPath
        )

        if ($ExplicitPath) {
            if (-not (Test-Path -LiteralPath $ExplicitPath)) {
                throw "$Name shim not found at $ExplicitPath"
            }
            return (Resolve-Path -LiteralPath $ExplicitPath).Path
        }

        $rubashRoot = Join-Path $RepoRoot "..\rubash"
        if (-not (Test-Path -LiteralPath (Join-Path $rubashRoot "Cargo.toml"))) {
            throw "$Name shim source not found. Pass -$($Name.Substring(0, 1).ToUpper())$($Name.Substring(1))ShimPath C:\path\to\$name.exe"
        }

        if ($Target) {
            $shimExe = Join-Path $rubashRoot "target\$Target\$Configuration\$name.exe"
        }
        else {
            $shimExe = Join-Path $rubashRoot "target\$Configuration\$name.exe"
        }
        if (-not (Test-Path -LiteralPath $shimExe)) {
            $buildArgs = @("build", "--manifest-path", (Join-Path $rubashRoot "Cargo.toml"), "--bin", $Name, "--locked")
            if ($Configuration -eq "release") {
                $buildArgs += "--release"
            }
            if ($Target) {
                $buildArgs += @("--target", $Target)
            }
            $previousRustFlags = $env:RUSTFLAGS
            try {
                $env:RUSTFLAGS = ""
                & cargo @buildArgs
                if ($LASTEXITCODE -ne 0) {
                    throw "Failed to build $Name shim."
                }
            }
            finally {
                if ($null -eq $previousRustFlags) {
                    Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue
                }
                else {
                    $env:RUSTFLAGS = $previousRustFlags
                }
            }
        }
        if (-not (Test-Path -LiteralPath $shimExe)) {
            throw "$name.exe not found at $shimExe"
        }
        return $shimExe
    }

    $bashShimExe = Resolve-RubashShim -Name "bash" -ExplicitPath $BashShimPath
    if ($ShShimPath) {
        $shShimExe = Resolve-RubashShim -Name "sh" -ExplicitPath $ShShimPath
    }
    else {
        # TODO(posix-mode): replace with a dedicated sh.exe shim if we decide
        # to make /bin/sh enter POSIX mode instead of matching bash behavior.
        $shShimExe = $bashShimExe
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
        Join-Path $RepoRoot "assets\niubash-icon.ico"
        Join-Path $RepoRoot "assets\niubash-icon-256.png"
        Join-Path $RepoRoot "assets\niubash-icon-64.png"
        Join-Path $RepoRoot "assets\niubash-icon.png"
    )
    foreach ($iconFile in $iconFiles) {
        if (-not (Test-Path -LiteralPath $iconFile)) {
            throw "Icon asset not found at $iconFile"
        }
    }

    $resolvedOhMyNiubashBundlePath = $null
    if (-not $SkipOhMyNiubashBundle) {
        if ($OhMyNiubashBundlePath) {
            $bundleCandidates = @($OhMyNiubashBundlePath)
        }
        else {
            $bundleCandidates = @(
                (Join-Path $RepoRoot "..\oh-my-niu")
                (Join-Path $RepoRoot "bundles\oh-my-niu")
                (Join-Path $RepoRoot "vendor\oh-my-niu")
            )
        }

        foreach ($candidate in $bundleCandidates) {
            if ((Test-Path -LiteralPath $candidate) -and (Test-Path -LiteralPath (Join-Path $candidate "bundle.toml"))) {
                $resolvedOhMyNiubashBundlePath = (Resolve-Path -LiteralPath $candidate).Path
                break
            }
        }

        if (-not $resolvedOhMyNiubashBundlePath) {
            throw "oh-my-niu bundle not found. Pass -OhMyNiubashBundlePath C:\path\to\oh-my-niu or -SkipOhMyNiubashBundle."
        }

        $bundleToml = Get-Content -LiteralPath (Join-Path $resolvedOhMyNiubashBundlePath "bundle.toml") -Raw
        $availableMatch = [regex]::Match($bundleToml, '(?ms)^\s*available\s*=\s*\[(.*?)\]')
        if (-not $availableMatch.Success) {
            throw "oh-my-niu bundle manifest has no [packs].available list: $resolvedOhMyNiubashBundlePath"
        }
        $availablePacks = [regex]::Matches($availableMatch.Groups[1].Value, '"([^"]+)"') |
            ForEach-Object { $_.Groups[1].Value }
        foreach ($packName in $availablePacks) {
            $packManifest = Join-Path $resolvedOhMyNiubashBundlePath (Join-Path "packs\$packName" "plugin.toml")
            $frameworkManifest = Join-Path $resolvedOhMyNiubashBundlePath (Join-Path "plugins\$packName" "plugin.toml")
            if (-not (Test-Path -LiteralPath $packManifest) -and -not (Test-Path -LiteralPath $frameworkManifest)) {
                throw "oh-my-niu bundle pack '$packName' is listed in bundle.toml but missing from packs/ and plugins/: $packManifest"
            }
        }
    }

    $distDir = Join-Path $RepoRoot "dist"
    if ($Arch) {
        $packageName = "niubash-v$Version-win-$Arch"
    }
    else {
        $packageName = "niubash-v$Version"
    }
    $stageDir = Join-Path $distDir $packageName
    $zipPath = Join-Path $distDir "$packageName.zip"

    Remove-Item -LiteralPath $stageDir -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $zipPath -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "winuxcmd\bin") | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "winuxcmd\usr\bin") | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $stageDir "assets") | Out-Null

    Copy-Item -LiteralPath $niubashExe -Destination (Join-Path $stageDir "niu.exe") -Force
    Copy-Item -LiteralPath $WinuxCmdPath -Destination (Join-Path $stageDir "winuxcmd\usr\bin\winuxcmd.exe") -Force
    Copy-Item -LiteralPath $bashShimExe -Destination (Join-Path $stageDir "winuxcmd\usr\bin\bash.exe") -Force
    Copy-Item -LiteralPath $shShimExe -Destination (Join-Path $stageDir "winuxcmd\usr\bin\sh.exe") -Force
    Copy-Item -LiteralPath $bashShimExe -Destination (Join-Path $stageDir "winuxcmd\bin\bash.exe") -Force
    Copy-Item -LiteralPath $shShimExe -Destination (Join-Path $stageDir "winuxcmd\bin\sh.exe") -Force
    Copy-Item -LiteralPath $activationScript -Destination (Join-Path $stageDir "winuxcmd\usr\bin\activate-winuxcmd.sh") -Force
    foreach ($iconFile in $iconFiles) {
        Copy-Item -LiteralPath $iconFile -Destination (Join-Path $stageDir "assets") -Force
    }
    if ($resolvedOhMyNiubashBundlePath) {
        $bundleStageDir = Join-Path $stageDir "bundles\oh-my-niu"
        New-Item -ItemType Directory -Force -Path $bundleStageDir | Out-Null
        $requiredBundleEntries = @(
            "oh-my-niu.winux"
            "bundle.toml"
            "index.toml"
            "lib"
            "plugins"
            "packs"
            "themes"
        )
        $bundleEntries = @(
            "oh-my-niu.winux"
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
            $source = Join-Path $resolvedOhMyNiubashBundlePath $entry
            if (Test-Path -LiteralPath $source) {
                Copy-Item -LiteralPath $source -Destination $bundleStageDir -Recurse -Force
            }
            elseif ($requiredBundleEntries -contains $entry) {
                throw "Required oh-my-niu bundle entry missing: $source"
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
