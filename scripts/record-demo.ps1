param(
    [string]$Ffmpeg = "",
    [int]$Seconds = 24
)
# Record the Niubash demo script running in a dedicated Windows Terminal
# window, then convert the capture to a GIF. Output lands in assets/.
$ErrorActionPreference = "Stop"

$demo = Join-Path $PSScriptRoot "..\assets\demo-script.sh"
$outMkv = Join-Path $PSScriptRoot "..\assets\demo-recording.mkv"
$outGif = Join-Path $PSScriptRoot "..\assets\demo.gif"

if (-not $Ffmpeg) {
    $pkg = Get-ChildItem "$env:LOCALAPPDATA\Microsoft\WinGet\Packages" -Directory -Filter "Gyan.FFmpeg*" -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($pkg) {
        $exe = Get-ChildItem $pkg.FullName -Recurse -Filter ffmpeg.exe -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if ($exe) { $Ffmpeg = $exe.FullName }
    }
}
if (-not $Ffmpeg -or -not (Test-Path $Ffmpeg)) { throw "ffmpeg not found; pass -Ffmpeg <path>" }

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class CapWin32 {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, System.Text.StringBuilder s, int n);
  [DllImport("shcore.dll")] public static extern int SetProcessDpiAwareness(int v);
  public struct RECT { public int Left, Top, Right, Bottom; }
}
"@
[void][CapWin32]::SetProcessDpiAwareness(2)  # per-monitor: GetWindowRect returns physical px

Remove-Item $outMkv, $outGif -ErrorAction SilentlyContinue

# 1. Close any leftover demo window, then launch a fresh Windows Terminal
#    window on the Niubash profile running the demo script.
Get-Process WindowsTerminal -ErrorAction SilentlyContinue |
    Where-Object { $_.MainWindowHandle -ne 0 -and $_.MainWindowTitle -match "Niubash" } |
    ForEach-Object { [void]$_.CloseMainWindow() }
Start-Sleep -Seconds 1
Start-Process wt.exe -ArgumentList "-p", "Niubash", "--", "niu.exe", "`"$demo`""

# 2. Find the terminal window by title and bring it to front.
$win = $null
for ($i = 0; $i -lt 30; $i++) {
    Start-Sleep -Milliseconds 500
    $win = Get-Process WindowsTerminal -ErrorAction SilentlyContinue |
        Where-Object { $_.MainWindowHandle -ne 0 -and $_.MainWindowTitle -match "Niubash" } |
        Select-Object -First 1
    if ($win) { break }
}
if (-not $win) { throw "Windows Terminal window with title 'Niubash' not found" }
Write-Host "capturing window: '$($win.MainWindowTitle)'"

$r = New-Object CapWin32+RECT
[void][CapWin32]::GetWindowRect($win.MainWindowHandle, [ref]$r)
[void][CapWin32]::SetForegroundWindow($win.MainWindowHandle)
$w = $r.Right - $r.Left
$h = $r.Bottom - $r.Top
Start-Sleep -Seconds 2
Write-Host "region: $($r.Left),$($r.Top) ${w}x${h}"

# 3. Record the region.
& $Ffmpeg -y -f gdigrab -framerate 24 -offset_x $r.Left -offset_y $r.Top `
    -video_size "${w}x${h}" -i desktop -t $Seconds -c:v libx264 -preset fast `
    -crf 18 -pix_fmt yuv420p $outMkv
if ($LASTEXITCODE -ne 0) { throw "ffmpeg capture failed" }

# 4. Convert to GIF with a palette pass.
& $Ffmpeg -y -i $outMkv -vf "fps=12,scale=880:-1:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=160:stats_mode=diff[p];[s1][p]paletteuse=dither=bayer:bayer_scale=3" -loop 0 $outGif
if ($LASTEXITCODE -ne 0) { throw "gif conversion failed" }

$m = (Get-Item $outMkv).Length / 1MB
$g = (Get-Item $outGif).Length / 1MB
Write-Host "done: mkv=$([math]::Round($m,1))MB gif=$([math]::Round($g,1))MB"
