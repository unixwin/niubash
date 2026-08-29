param(
    [string]$Ffmpeg = "",
    [int]$Seconds = 16
)
# Record a Git Bash (MSYS) session showing the commands that FAIL there but
# work in Niubash: backslash paths eaten, /c/ paths rejected, MSYS path
# conversion silently breaking git grep. Output: assets/demo-gitbash.gif
$ErrorActionPreference = "Stop"

$outMkv = Join-Path $PSScriptRoot "..\assets\demo-gitbash.mkv"
$outGif = Join-Path $PSScriptRoot "..\assets\demo-gitbash.gif"

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
public class GbWin32 {
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
[void][GbWin32]::SetProcessDpiAwareness(2)

function Find-GitBashWindow {
    # Git Bash interactive windows title themselves "MINGW64:/path".
    $script:found = [IntPtr]::Zero
    $cb = [GbWin32+EnumProc]{
        param($h, $l)
        if ([GbWin32]::IsWindowVisible($h)) {
            $sb = New-Object System.Text.StringBuilder 512
            [void][GbWin32]::GetWindowText($h, $sb, 512)
            if ($sb.ToString().StartsWith("MINGW64:")) { $script:found = $h }
        }
        return $true
    }
    [void][GbWin32]::EnumWindows($cb, [IntPtr]::Zero)
    return $script:found
}

function Activate-DemoWindow {
    for ($i = 0; $i -lt 8; $i++) {
        [void][GbWin32]::ShowWindow($hwnd, 9)
        [void][GbWin32]::SetForegroundWindow($hwnd)
        Start-Sleep -Milliseconds 250
        if ([GbWin32]::GetForegroundWindow() -eq $hwnd) { return $true }
        $tid = [GbWin32]::GetWindowThreadProcessId($hwnd, [ref]0)
        [void][GbWin32]::AttachThreadInput([GbWin32]::GetCurrentThreadId(), $tid, $true)
        [void][GbWin32]::SetForegroundWindow($hwnd)
        [void][GbWin32]::AttachThreadInput([GbWin32]::GetCurrentThreadId(), $tid, $false)
        Start-Sleep -Milliseconds 250
        if ([GbWin32]::GetForegroundWindow() -eq $hwnd) { return $true }
    }
    return $false
}

Remove-Item $outMkv, $outGif -ErrorAction SilentlyContinue

# 1. Close our own leftover demo window, then launch a fresh Git Bash in WT.
$old = Find-GitBashWindow
if ($old -ne [IntPtr]::Zero) { [void][GbWin32]::PostMessage($old, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) }
Start-Sleep -Seconds 1
Start-Process wt.exe -ArgumentList "-w", "new", "--", "C:\Progra~1\Git\bin\bash.exe", "-i"

# 2. Wait for the Git Bash window.
$hwnd = [IntPtr]::Zero
for ($i = 0; $i -lt 40; $i++) {
    Start-Sleep -Milliseconds 500
    $hwnd = Find-GitBashWindow
    if ($hwnd -ne [IntPtr]::Zero) { break }
}
if ($hwnd -eq [IntPtr]::Zero) { throw "Git Bash window not found" }
Write-Host "window found: hwnd=$hwnd"

$r = New-Object GbWin32+RECT
[void][GbWin32]::GetWindowRect($hwnd, [ref]$r)
$w = $r.Right - $r.Left
$h = $r.Bottom - $r.Top
Start-Sleep -Seconds 4
Write-Host "capturing: ${w}x${h}"

$ws = New-Object -ComObject WScript.Shell
Add-Type -AssemblyName System.Windows.Forms
function Send-Line([string]$text, [int]$waitMs = 1700) {
    if (-not (Activate-DemoWindow)) { Write-Warning "could not focus demo window for: $text" }
    Start-Sleep -Milliseconds 200
    [System.Windows.Forms.Clipboard]::SetText($text)
    Start-Sleep -Milliseconds 150
    $ws.SendKeys("^+v")
    Start-Sleep -Milliseconds 250
    $ws.SendKeys("{ENTER}")
    Start-Sleep -Milliseconds $waitMs
}

# 3. Record while driving the session.
$job = Start-Job -ArgumentList $Ffmpeg, $outMkv, $r.Left, $r.Top, $w, $h, $Seconds {
    param($ff, $out, $x, $y, $ww, $hh, $sec)
    & $ff -y -f gdigrab -framerate 24 -offset_x $x -offset_y $y -video_size "${ww}x${hh}" `
        -i desktop -t $sec -c:v libx264 -preset fast -crf 18 -pix_fmt yuv420p $out 2>$null
}

Start-Sleep -Seconds 3
Send-Line 'node -e "console.log(JSON.stringify(process.argv.slice(3)))" "C:\Users\caomengxuan\repo\niubash"' 2000   # backslash path eaten
Send-Line 'cmd /c "dir /c/Windows/System32/drivers/etc"' 2000   # /c/ rejected
Send-Line 'git grep "/fn main"' 2000   # MSYS path conversion breaks the pattern
Send-Line 'git grep -n "fn main" | head -3' 2400   # same search, normal quoting: works
Send-Line "exit" 500

Wait-Job $job | Out-Null
Receive-Job $job | Out-Null
Remove-Job $job
if (-not (Test-Path $outMkv)) { throw "ffmpeg capture failed" }

$old = Find-GitBashWindow
if ($old -ne [IntPtr]::Zero) { [void][GbWin32]::PostMessage($old, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) }

& $Ffmpeg -y -i $outMkv -vf "fps=12,scale=880:-1:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=160:stats_mode=diff[p];[s1][p]paletteuse=dither=bayer:bayer_scale=3" -loop 0 $outGif
if ($LASTEXITCODE -ne 0) { throw "gif conversion failed" }

$m = (Get-Item $outMkv).Length / 1MB
$g = (Get-Item $outGif).Length / 1MB
Write-Host "done: mkv=$([math]::Round($m,1))MB gif=$([math]::Round($g,1))MB"
