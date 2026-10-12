param(
    [string]$Version,
    [string]$WinuxCmdPath,
    [string]$Configuration = "release",
    [string]$Target,
    [string]$Arch,
    [string]$BashShimPath,
    [string]$ShShimPath,
    # Staged wpm pre-install root (niubash#189): a WinuxCmd root whose
    # usr\bin holds the pre-install shims (gawk.exe, awk.exe) and whose opt\
    # holds the package payloads with their private DLLs. Its usr\bin *.exe
    # (except winuxcmd.exe itself, which is staged from -WinuxCmdPath) and
    # its whole opt\ tree are copied into the package's winuxcmd\ directory,
    # so the shim dispatch (<root>\usr\bin\<cmd>.exe -> <root>\opt\<pkg>\)
    # resolves inside the shipped layout. Never ships the .wpm state dir.
    [string]$PreinstallRoot,
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
        # The shim binary self-identifies through its own argv[0]: copied as
        # sh.exe it forwards the invocation name (NIU_INVOKED_AS) to niu, which
        # then enters POSIX mode and shows $0 = sh (GNU shell.c:733-737, the
        # sh-posix contract). The same bytes as bash.exe forward no name and
        # stay in the default bash-compatible mode.
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

    # The built-in plugin/theme stack — including the oh-my-niu bundle —
    # is retired (niubash#145/#161): the product's plugin verbs hard-bail
    # on bundles, so release packages no longer stage one. The external
    # bash ecosystem installs on demand through `niu plugin add` sources.

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

    # niubash#189: pre-installed wpm packages (see -PreinstallRoot above).
    if ($PreinstallRoot) {
        if (-not (Test-Path -LiteralPath (Join-Path $PreinstallRoot "usr\bin"))) {
            throw "PreinstallRoot has no usr\bin: $PreinstallRoot"
        }
        $preinstallShims = Get-ChildItem -LiteralPath (Join-Path $PreinstallRoot "usr\bin") -Filter *.exe |
            Where-Object { $_.Name -ne "winuxcmd.exe" }
        foreach ($shim in $preinstallShims) {
            Copy-Item -LiteralPath $shim.FullName -Destination (Join-Path $stageDir "winuxcmd\usr\bin\$($shim.Name)") -Force
        }
        if (Test-Path -LiteralPath (Join-Path $PreinstallRoot "opt")) {
            Copy-Item -LiteralPath (Join-Path $PreinstallRoot "opt") -Destination (Join-Path $stageDir "winuxcmd\opt") -Recurse -Force
        }
        if ($preinstallShims.Count -gt 0) {
            Write-Host "Pre-installed shims: $($preinstallShims.Name -join ', ')"
        }
        else {
            Write-Warning "PreinstallRoot was set but usr\bin has no shims; shipping without pre-installed packages (fail-open)"
        }
    }

    foreach ($iconFile in $iconFiles) {
        Copy-Item -LiteralPath $iconFile -Destination (Join-Path $stageDir "assets") -Force
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
