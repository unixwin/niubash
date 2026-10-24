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
    [switch]$AllowPathWinuxCmd,
    # niubash#470: skip the staged-tree hardlink alignment and the
    # hardlink-native archives (.tar.gz / .7z), producing only the classic
    # zip. Escape hatch for callers that want the legacy output shape.
    [switch]$SkipHardlinkArchives
)

$ErrorActionPreference = "Stop"

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Push-Location $RepoRoot
try {
    # niubash#470: re-link byte-identical staged files as NTFS hardlinks so
    # the staged tree matches what `wpm install` produces on a real disk.
    # Deterministic: the lexicographically first path of each content group
    # becomes the link target, so re-running the packager yields the same
    # layout. Content-neutral by construction (only SHA256-equal files are
    # linked), and degrading gracefully: on a filesystem without hardlink
    # support the copies simply stay copies, with a loud warning — every
    # downstream consumer (zip, setup.exe, smoke gates) is unaffected.
    function Complete-StagedHardlinks {
        param([string]$StageDir)

        $groups = @{}
        foreach ($file in (Get-ChildItem -LiteralPath $StageDir -Recurse -File)) {
            $hash = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash
            if (-not $groups.ContainsKey($hash)) {
                $groups[$hash] = [System.Collections.Generic.List[string]]::new()
            }
            $groups[$hash].Add($file.FullName)
        }

        $families = 0
        $links = 0
        $saved = [long]0
        foreach ($group in $groups.Values) {
            if ($group.Count -lt 2) { continue }
            $sorted = $group | Sort-Object { $_ }
            $primary = $sorted[0]
            $primaryLength = (Get-Item -LiteralPath $primary).Length
            $families++
            foreach ($duplicate in ($sorted | Select-Object -Skip 1)) {
                try {
                    New-Item -ItemType HardLink -Path $duplicate -Target $primary -Force -ErrorAction Stop | Out-Null
                    $links++
                    $saved += $primaryLength
                }
                catch {
                    # No hardlink support on the staging volume (FAT32, some
                    # network shares): ship the independent copies rather
                    # than fail the release. The archives stay correct — the
                    # tar.gz just stores full entries and keeps its unlinked
                    # size.
                    Write-Warning "hardlink for '$duplicate' failed ($($_.Exception.Message)); keeping an independent copy"
                }
            }
        }
        if ($families -gt 0) {
            Write-Host ("Staged hardlinks: {0} duplicate-content families re-linked ({1} links, {2} MB of duplicate payload bytes now shared)" -f `
                $families, $links, [Math]::Round($saved / 1MB, 2))
        }
        else {
            Write-Host "Staged hardlinks: no duplicate-content families found"
        }
    }

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

    function Resolve-ShellShim {
        param(
            [string]$ExplicitPath
        )

        if ($ExplicitPath) {
            return Resolve-RubashShim -Name "bash" -ExplicitPath $ExplicitPath
        }

        # niubash#464: the forwarding shim is niu's own binary now (the
        # `bash` bin in this repository). Build it from this tree; the
        # borrowed rubash build above stays as the fallback, so a broken
        # shim build can never block a release the old way still covers.
        try {
            if ($Target) {
                $ownShimExe = Join-Path $RepoRoot "target\$Target\$Configuration\bash.exe"
            }
            else {
                $ownShimExe = Join-Path $RepoRoot "target\$Configuration\bash.exe"
            }
            if (-not (Test-Path -LiteralPath $ownShimExe)) {
                $buildArgs = @("build", "--manifest-path", (Join-Path $RepoRoot "Cargo.toml"), "--bin", "bash", "--locked")
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
                        throw "Failed to build the niu bash shim."
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
            if (-not (Test-Path -LiteralPath $ownShimExe)) {
                throw "bash.exe not found at $ownShimExe"
            }
            return $ownShimExe
        }
        catch {
            Write-Warning "niubash#464: building the niu-owned bash shim failed ($($_.Exception.Message)); falling back to the rubash-built shim."
            return Resolve-RubashShim -Name "bash" -ExplicitPath $null
        }
    }

    $bashShimExe = Resolve-ShellShim -ExplicitPath $BashShimPath
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
    # niubash#464: one canonical shim payload, staged under its own name.
    # `niu --relink-shells` copies these bytes to a bash/sh face wherever a
    # hardlink cannot exist (exFAT, network shares); the face copies above
    # are the package layout for volumes that never run the relink.
    Copy-Item -LiteralPath $bashShimExe -Destination (Join-Path $stageDir "winuxcmd\usr\bin\niu-shim.exe") -Force
    Copy-Item -LiteralPath $activationScript -Destination (Join-Path $stageDir "winuxcmd\usr\bin\activate-winuxcmd.sh") -Force

    # niubash#189: pre-installed wpm packages (see -PreinstallRoot above).
    # The same usr\bin sweep also ships `winuxcmd-shim.exe` (the package-shim
    # forwarder, staged by the release workflow after the wpm installs) so
    # upgrades replace a stale forwarder and runtime `wpm install` can link
    # new package shims to it (niubash#466/#468).
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

    # niubash#470: restore the real install layout before any archive is
    # written. wpm installs hardlink their payloads, but the Copy-Item sweep
    # above flattens every link into an independent full copy (PowerShell
    # Copy-Item never preserves hardlinks), and Compress-Archive would ship
    # each copy in full — the v1.4.2 x64 zip carried 6 independent copies of
    # git.exe (34.7 MB of the 57.1 MB archive was duplicated payload bytes).
    # Re-linking byte-identical files is content-neutral: every path keeps
    # the same bytes it already had, so the zip, the Inno setup.exe, the
    # staged-package smoke gates, and any later `wpm` reinstall behave
    # exactly as before. What changes: the hardlink-native archives below
    # (.tar.gz via bsdtar) can then store each shared family once, and the
    # extracted tree keeps the small on-disk footprint a `wpm install` would
    # have produced. Every staged path stays a complete real file — hardlinks
    # only share storage — so an unpacked tree is complete by construction:
    # no stand-in files, no manifests, no relink step for users.
    if (-not $SkipHardlinkArchives) {
        Complete-StagedHardlinks -StageDir $stageDir
    }

    Compress-Archive -LiteralPath $stageDir -DestinationPath $zipPath -Force

    $files = Get-ChildItem -LiteralPath $stageDir -Recurse -File
    $size = (Get-Item -LiteralPath $zipPath).Length
    Write-Host "Created $zipPath"
    Write-Host "Files: $($files.Count)"
    Write-Host "Zip size: $([Math]::Round($size / 1MB, 2)) MB"

    # niubash#470: hardlink-native companions to the zip. The staged tree
    # above now carries real NTFS hardlinks, and bsdtar (the Windows built-in
    # tar.exe) records them as tar hardlink entries and restores them on
    # extraction — measured on the v1.4.2 tree: 54.47 MB zip -> 29.63 MB
    # tar.gz with the git/awk families re-linked after unpack. 7-Zip gets the
    # size win from solid compression alone (13.09 MB measured); -snh is kept
    # for intent but 7-Zip 26.04 neither stores nor restores NTFS hardlinks
    # with it, so the .7z archives content, not links — documented in the
    # release body, verified as content-identity in the workflow.
    if (-not $SkipHardlinkArchives) {
        $tarGzPath = Join-Path $distDir "$packageName.tar.gz"
        Remove-Item -LiteralPath $tarGzPath -Force -ErrorAction SilentlyContinue
        tar -czf $tarGzPath -C $distDir $packageName
        if ($LASTEXITCODE -ne 0) {
            throw "tar.exe failed to create $tarGzPath (exit $LASTEXITCODE)"
        }
        Write-Host "Tar.gz size: $([Math]::Round((Get-Item -LiteralPath $tarGzPath).Length / 1MB, 2)) MB"

        $sevenZipCandidates = @()
        if ($env:ProgramFiles) { $sevenZipCandidates += Join-Path $env:ProgramFiles "7-Zip\7z.exe" }
        if (${env:ProgramFiles(x86)}) { $sevenZipCandidates += Join-Path ${env:ProgramFiles(x86)} "7-Zip\7z.exe" }
        $sevenZip = $sevenZipCandidates |
            Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
        if (-not $sevenZip) {
            $sevenZip = (Get-Command 7z.exe -ErrorAction SilentlyContinue).Source
        }
        if ($sevenZip) {
            $sevenZipPath = Join-Path $distDir "$packageName.7z"
            Remove-Item -LiteralPath $sevenZipPath -Force -ErrorAction SilentlyContinue
            & $sevenZip a -snh -mx=9 $sevenZipPath (Join-Path $distDir $packageName) | Out-Null
            if ($LASTEXITCODE -ne 0) {
                throw "7z failed to create $sevenZipPath (exit $LASTEXITCODE)"
            }
            Write-Host "7z size: $([Math]::Round((Get-Item -LiteralPath $sevenZipPath).Length / 1MB, 2)) MB"
        }
        else {
            # Local convenience builds may lack 7-Zip; the release workflow
            # hard-asserts the .7z exists, so a CI leg can never silently
            # lose the asset.
            Write-Warning "7-Zip not found; skipping the .7z archive (the .zip and .tar.gz are complete)"
        }
    }

    Write-Host "Contents:"
    $files | ForEach-Object {
        Write-Host "  $($_.FullName.Substring($stageDir.Length + 1))"
    }
}
finally {
    Pop-Location
}
