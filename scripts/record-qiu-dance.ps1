param(
    [string]$Ffmpeg = "",
    [int]$Seconds = 12,
    [string]$Hwnd = ""
)
# Record the user's already-open Niubash terminal playing an animated
# termflag (./flags/qiu-dance.sh), then convert to a GIF.
# The window is NEVER closed or moved - we only capture its region.
$ErrorActionPreference = "Stop"

$outMkv = Join-Path $PSScriptRoot "..\..\terminal-flags\.ci-output\qiu-dance-demo.mkv"
$outGif = Join-Path $PSScriptRoot "..\..\terminal-flags\.ci-output\qiu-dance-demo.gif"

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
using System.Text;
using System.Runtime.InteropServices;
public class QiuWin32 {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern uint GetCurrentThreadId();
  [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool attach);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("shcore.dll")] public static extern int SetProcessDpiAwareness(int v);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  public struct RECT { public int Left, Top, Right, Bottom; }
}
"@
[void][QiuWin32]::SetProcessDpiAwareness(2)

# Resolve the target window: explicit -Hwnd, else exact title "Niubash".
$targetHwnd = [IntPtr]::Zero
if ($Hwnd) {
    $targetHwnd = New-Object System.IntPtr([long]$Hwnd)
} else {
    $script:found = [IntPtr]::Zero
    $cb = [QiuWin32+EnumProc]{
        param($h, $l)
        if ([QiuWin32]::IsWindowVisible($h)) {
            $sb = New-Object System.Text.StringBuilder 512
            [void][QiuWin32]::GetWindowText($h, $sb, 512)
            if ($sb.ToString().Trim() -eq "Niubash") { $script:found = $h }
        }
        return $true
    }
    [void][QiuWin32]::EnumWindows($cb, [IntPtr]::Zero)
    $targetHwnd = $script:found
}
if ($targetHwnd -eq [IntPtr]::Zero) { throw "target window not found (pass -Hwnd)" }
Write-Host "target window: $targetHwnd"

# Activate with verification (never close/move the window).
for ($i = 0; $i -lt 8; $i++) {
    [void][QiuWin32]::ShowWindow($targetHwnd, 9)
    [void][QiuWin32]::SetForegroundWindow($targetHwnd)
    Start-Sleep -Milliseconds 250
    if ([QiuWin32]::GetForegroundWindow() -eq $targetHwnd) { break }
    $tid = [QiuWin32]::GetWindowThreadProcessId($targetHwnd, [ref]0)
    [void][QiuWin32]::AttachThreadInput([QiuWin32]::GetCurrentThreadId(), $tid, $true)
    [void][QiuWin32]::SetForegroundWindow($targetHwnd)
    [void][QiuWin32]::AttachThreadInput([QiuWin32]::GetCurrentThreadId(), $tid, $false)
    Start-Sleep -Milliseconds 250
}

$r = New-Object QiuWin32+RECT
[void][QiuWin32]::GetWindowRect($targetHwnd, [ref]$r)
$w = $r.Right - $r.Left
$h = $r.Bottom - $r.Top
Write-Host "region: $($r.Left),$($r.Top) ${w}x${h}"

Remove-Item $outMkv, $outGif -ErrorAction SilentlyContinue

$job = Start-Job -ArgumentList $Ffmpeg, $outMkv, $r.Left, $r.Top, $w, $h, $Seconds {
    param($ff, $out, $x, $y, $ww, $hh, $sec)
    & $ff -y -f gdigrab -framerate 24 -offset_x $x -offset_y $y -video_size "${ww}x${hh}" `
        -i desktop -t $sec -c:v libx264 -preset fast -crf 18 -pix_fmt yuv420p $out 2>$null
}

# Drive the command into the terminal via clipboard paste (Ctrl+Shift+V).
Start-Sleep -Seconds 2
$ws = New-Object -ComObject WScript.Shell
Add-Type -AssemblyName System.Windows.Forms
[System.Windows.Forms.Clipboard]::SetText("./flags/qiu-dance.sh")
Start-Sleep -Milliseconds 200
$ws.SendKeys("^+v")
Start-Sleep -Milliseconds 300
$ws.SendKeys("{ENTER}")

Wait-Job $job | Out-Null
Receive-Job $job | Out-Null
Remove-Job $job
if (-not (Test-Path $outMkv)) { throw "ffmpeg capture failed" }

& $Ffmpeg -y -i $outMkv -vf "fps=12,scale=560:-1:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=160:stats_mode=diff[p];[s1][p]paletteuse=dither=bayer:bayer_scale=3" -loop 0 $outGif
if ($LASTEXITCODE -ne 0) { throw "gif conversion failed" }

$m = (Get-Item $outMkv).Length / 1MB
$g = (Get-Item $outGif).Length / 1MB
Write-Host "done: mkv=$([math]::Round($m,1))MB gif=$([math]::Round($g,1))MB"
