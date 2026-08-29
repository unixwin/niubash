param(
    [string]$Ffmpeg = "",
    [int]$Seconds = 22,
    [switch]$Probe
)
# Record an INTERACTIVE niubash session (real user config: theme + git prompt)
# in a dedicated Windows Terminal window. Uses the real HOME so themes and
# plugin state load exactly like a normal interactive session.
$ErrorActionPreference = "Stop"

$outMkv = Join-Path $PSScriptRoot "..\assets\demo-interactive.mkv"
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
using System.Text;
using System.Runtime.InteropServices;
public class CapWin32 {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern uint GetCurrentThreadId();
  [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool attach);
  [DllImport("shcore.dll")] public static extern int SetProcessDpiAwareness(int v);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  public struct RECT { public int Left, Top, Right, Bottom; }
}
"@
[void][CapWin32]::SetProcessDpiAwareness(2)

function Find-NiubashWindow {
    # WT hosts multiple windows inside one WindowsTerminal process, so
    # Get-Process/MainWindowHandle cannot see the extra windows. Enumerate all
    # top-level windows and match the exact title instead.
    $script:found = [IntPtr]::Zero
    $cb = [CapWin32+EnumProc]{
        param($h, $l)
        if ([CapWin32]::IsWindowVisible($h)) {
            $sb = New-Object System.Text.StringBuilder 512
            [void][CapWin32]::GetWindowText($h, $sb, 512)
            if ($sb.ToString().Trim() -eq "Niubash") { $script:found = $h }
        }
        return $true
    }
    [void][CapWin32]::EnumWindows($cb, [IntPtr]::Zero)
    return $script:found
}

function Close-NiubashWindow {
    $h = Find-NiubashWindow
    if ($h -ne [IntPtr]::Zero) {
        [void][CapWin32]::PostMessage($h, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)  # WM_CLOSE
    }
}

Remove-Item $outMkv, $outGif -ErrorAction SilentlyContinue

# 1. Close any leftover demo window (exact title only; never touches other
#    windows, e.g. the "OC | niubash ..." host window), then open a fresh one.
Close-NiubashWindow
Start-Sleep -Seconds 1
Start-Process wt.exe -ArgumentList "-w", "new", "-p", "Niubash"

# 2. Wait for the new window (title is exactly "Niubash").
$hwnd = [IntPtr]::Zero
for ($i = 0; $i -lt 40; $i++) {
    Start-Sleep -Milliseconds 500
    $hwnd = Find-NiubashWindow
    if ($hwnd -ne [IntPtr]::Zero) { break }
}
if ($hwnd -eq [IntPtr]::Zero) { throw "Niubash window not found (enumerate failed)" }
Write-Host "window found: hwnd=$hwnd"

$r = New-Object CapWin32+RECT
[void][CapWin32]::GetWindowRect($hwnd, [ref]$r)
[void][CapWin32]::SetForegroundWindow($hwnd)
$w = $r.Right - $r.Left
$h = $r.Bottom - $r.Top
Start-Sleep -Seconds 3
Write-Host "capturing: ${w}x${h} at $($r.Left),$($r.Top)"

if ($Probe) {
    # Probe mode: grab a single frame so we can verify the theme rendered.
    & $Ffmpeg -y -f gdigrab -framerate 5 -offset_x $r.Left -offset_y $r.Top `
        -video_size "${w}x${h}" -i desktop -frames:v 1 `
        (Join-Path $PSScriptRoot "..\assets\demo-probe.png") 2>$null
    Close-NiubashWindow
    Write-Host "probe saved to assets\demo-probe.png"
    exit 0
}

$ws = New-Object -ComObject WScript.Shell
Add-Type -AssemblyName System.Windows.Forms
function Activate-DemoWindow {
    # Windows blocks background processes from stealing focus; verify with
    # GetForegroundWindow and fall back to the AttachThreadInput trick.
    for ($i = 0; $i -lt 8; $i++) {
        [void][CapWin32]::ShowWindow($hwnd, 9)          # SW_RESTORE
        [void][CapWin32]::SetForegroundWindow($hwnd)
        Start-Sleep -Milliseconds 250
        if ([CapWin32]::GetForegroundWindow() -eq $hwnd) { return $true }
        $tid = [CapWin32]::GetWindowThreadProcessId($hwnd, [ref]0)
        [void][CapWin32]::AttachThreadInput([CapWin32]::GetCurrentThreadId(), $tid, $true)
        [void][CapWin32]::SetForegroundWindow($hwnd)
        [void][CapWin32]::AttachThreadInput([CapWin32]::GetCurrentThreadId(), $tid, $false)
        Start-Sleep -Milliseconds 250
        if ([CapWin32]::GetForegroundWindow() -eq $hwnd) { return $true }
    }
    return $false
}

function Send-Line([string]$text, [int]$waitMs = 1700) {
    # Paste via clipboard (Ctrl+Shift+V in Windows Terminal) so input bypasses
    # any active IME. Only proceed when the demo window holds focus.
    if (-not (Activate-DemoWindow)) { Write-Warning "could not focus demo window for: $text" }
    Start-Sleep -Milliseconds 200
    # WinForms clipboard sets the exact text: Set-Clipboard would append a
    # CRLF, which corrupts heredoc terminators like "EOF" -> "EOF\r".
    [System.Windows.Forms.Clipboard]::SetText($text)
    Start-Sleep -Milliseconds 150
    $ws.SendKeys("^+v")
    Start-Sleep -Milliseconds 250
    $ws.SendKeys("{ENTER}")
    Start-Sleep -Milliseconds $waitMs
}

# 3. Record while driving the interactive session.
$job = Start-Job -ArgumentList $Ffmpeg, $outMkv, $r.Left, $r.Top, $w, $h, $Seconds {
    param($ff, $out, $x, $y, $ww, $hh, $sec)
    & $ff -y -f gdigrab -framerate 24 -offset_x $x -offset_y $y -video_size "${ww}x${hh}" `
        -i desktop -t $sec -c:v libx264 -preset fast -crf 18 -pix_fmt yuv420p $out 2>$null
}

Start-Sleep -Seconds 4          # let the prompt render
$work = "C:/Users/caomengxuan/AppData/Local/Temp/opencode/niubash-demo-work"
Send-Line "cd C:/Users/caomengxuan/repo/niubash" 1600   # git prompt appears
Send-Line "git status --short" 1400
Send-Line "cd $work" 1400
Send-Line "cat heroes.txt" 1200
Send-Line "grep -in hero heroes.txt" 1400               # colored git-prompt grep
Send-Line 'sed -i "s/world/niubash/g; s/World/NIUBASH/g" heroes.txt' 1500  # in-place edit
Send-Line "cat heroes.txt" 1200
Send-Line 'awk "{print \$2}" access.log | sort | uniq -c | sort -rn' 1700  # log stats
Send-Line "wc -l heroes.txt" 1100
Send-Line "tree -L 2" 1500
Send-Line "exit" 400

Wait-Job $job | Out-Null
Receive-Job $job | Out-Null
Remove-Job $job
if (-not (Test-Path $outMkv)) { throw "ffmpeg capture failed" }

# Close only the window this script launched.
Close-NiubashWindow

# 4. Convert to GIF with a palette pass.
& $Ffmpeg -y -i $outMkv -vf "fps=12,scale=880:-1:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=160:stats_mode=diff[p];[s1][p]paletteuse=dither=bayer:bayer_scale=3" -loop 0 $outGif
if ($LASTEXITCODE -ne 0) { throw "gif conversion failed" }

$m = (Get-Item $outMkv).Length / 1MB
$g = (Get-Item $outGif).Length / 1MB
Write-Host "done: mkv=$([math]::Round($m,1))MB gif=$([math]::Round($g,1))MB"
