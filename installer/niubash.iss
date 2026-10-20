#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

#ifndef AppArch
  #define AppArch "x64"
#endif

#ifndef SourceDir
  #define SourceDir "dist\niubash-v" + AppVersion + "-win-" + AppArch
#endif

#ifndef OutputDir
  #define OutputDir "dist"
#endif

#define AppName "Niubash"
#define AppPublisher "Unixwin"
#define AppUrl "https://github.com/unixwin/niubash"
#define AppExeName "niu.exe"
#define AppId "{{7D8B7341-8F4D-4C56-91F0-4AD220D41DB1}"

[Setup]
AppId={#AppId}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
AppUpdatesURL={#AppUrl}/releases
DefaultDirName={localappdata}\Programs\Niubash
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir={#OutputDir}
OutputBaseFilename=niubash-v{#AppVersion}-win-{#AppArch}-setup
SetupIconFile=..\assets\niubash-icon.ico
UninstallDisplayIcon={app}\{#AppExeName}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
ChangesEnvironment=yes
CloseApplications=yes
CloseApplicationsFilter=niu.exe
RestartApplications=no
#if AppArch == "arm64"
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#else
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "path"; Description: "Add Niubash to my user PATH"; Flags: checkedonce
Name: "wtprofile"; Description: "Add or update the Windows Terminal profile"; Flags: checkedonce
; niubash#211: the desktop shortcut exists so the WT form has something to
; upgrade; default checked — the WT experience is the owner-preferred default.
; niubash#313 (R3) disclosure: this task name is new in 1.4.x. Silent installs
; and upgrades (/SILENT, /VERYSILENT) never show the task page, and Inno's
; UsePreviousTasks default has no saved selection for a NEW task name — so
; silent upgrades of 1.3.x machines run this task with its default (checked)
; state and GROW a desktop icon without being asked. Owner ruling: the
; behavior is intended, and it is disclosed in the release notes
; (CHANGELOG Unreleased); interactive installs keep remembering an unchecked
; choice.
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: checkedonce

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

; niubash#313 (R4): [Files] overwrites but never removes — a file an earlier
; build shipped and the current payload retired survives every upgrade (and,
; sitting in {app} after uninstall, keeps the directory alive). Sweep the
; known retired names before extraction; new retirements add a line here.
[InstallDelete]
; The reverted dual-name build (071d16e) shipped niu.exe AND niubash.exe; the
; revert (0999540) stopped shipping the second binary but never removed it
; from upgraded machines.
Type: files; Name: "{app}\niubash.exe"
; Self-update-era stray rollback backups observed on real machines; dead
; weight once a full installer payload replaces the tree.
Type: files; Name: "{app}\niu.exe.old-*"
; niubash#376 (hygiene rule): {app}\winuxcmd\bin is THE PATH entry directory,
; and dead payloads inside it make the active entry set unauditable. Earlier
; updaters parked retired faces aside and never swept them (observed live:
; bash.exe.bak-1.4.0, sh.exe.old-shposix, 226,816 B each); the current
; relinker parks as <name>.niu-old-<pid> and drops it, but a crash between
; the two steps leaves the parked shape behind. PATHEXT matches none of
; these, so nothing is shadowed — sweep the retired-name shapes anyway,
; never the live entry names. The wildcards match generated shapes only;
; a user file deliberately named with one of these suffixes inside these
; two app-owned directories would be swept with them.
Type: files; Name: "{app}\winuxcmd\bin\*.bak-*"
Type: files; Name: "{app}\winuxcmd\bin\*.old-*"
Type: files; Name: "{app}\winuxcmd\bin\*.niu-old-*"
Type: files; Name: "{app}\winuxcmd\usr\bin\*.bak-*"
Type: files; Name: "{app}\winuxcmd\usr\bin\*.old-*"
Type: files; Name: "{app}\winuxcmd\usr\bin\*.niu-old-*"

; niubash#313 (R4): the same strays must not survive uninstall — processed
; after the uninstall log replay, so they cannot pin {app} behind.
[UninstallDelete]
Type: files; Name: "{app}\niubash.exe"
Type: files; Name: "{app}\niu.exe.old-*"
Type: files; Name: "{app}\winuxcmd\bin\*.bak-*"
Type: files; Name: "{app}\winuxcmd\bin\*.old-*"
Type: files; Name: "{app}\winuxcmd\bin\*.niu-old-*"
Type: files; Name: "{app}\winuxcmd\usr\bin\*.bak-*"
Type: files; Name: "{app}\winuxcmd\usr\bin\*.old-*"
Type: files; Name: "{app}\winuxcmd\usr\bin\*.niu-old-*"

; niubash#211: these are the conhost-baseline forms (direct niu.exe — what
; Win10 hosts with conhost). When WT is detected after install, the
; --install-wt-shortcuts [Run] step rewrites the SAME link files in place to
; `wt.exe -p Niubash`; same paths keeps Inno's uninstall log authoritative.
[Icons]
Name: "{group}\Niubash"; Filename: "{app}\{#AppExeName}"; WorkingDir: "{%USERPROFILE}"; IconFilename: "{app}\assets\niubash-icon.ico"
Name: "{autodesktop}\Niubash"; Filename: "{app}\{#AppExeName}"; WorkingDir: "{%USERPROFILE}"; IconFilename: "{app}\assets\niubash-icon.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\winuxcmd\usr\bin\winuxcmd.exe"; Parameters: "wpm links rebuild --root ""{app}\winuxcmd"" --force"; WorkingDir: "{app}\winuxcmd\usr\bin"; StatusMsg: "Activating WinuxCmd command links..."; Flags: runhidden waituntilterminated
; unixwin/niubash#468: `wpm links rebuild` reconciles only what `.wpm` state
; and the builtin applet table declare, and the bundled install ships no
; .wpm state — so the package shims (usr\bin\awk.exe, gawk.exe) are outside
; its set. An upgrade replaces winuxcmd.exe under its own name and leaves
; those links on the previous image's inode (1.4.1 shipped this as stale
; 1.1.7-era awk.exe/gawk.exe). niu owns the bundled shim set: this step
; parks and relinks every declared name that is not the current image, and
; upgrades extracted copies to real hardlinks.
Filename: "{app}\{#AppExeName}"; Parameters: "--relink-bundled-shims --force --quiet"; WorkingDir: "{app}"; StatusMsg: "Relinking bundled command shims..."; Flags: runhidden waituntilterminated
; unixwin/niubash#377: the shell faces become hardlinks to the real binary
; instead of the forwarding shims the package ships (one process saved per
; `bash -c`/`sh -c`). Idempotent; also refreshes names left on the previous
; inode when an update replaced niu.exe. The shims stay the fallback wherever
; hardlinks are unavailable.
Filename: "{app}\{#AppExeName}"; Parameters: "--relink-shells --force --quiet"; WorkingDir: "{app}"; StatusMsg: "Linking bash and sh to the shell..."; Flags: runhidden waituntilterminated
Filename: "{app}\{#AppExeName}"; Parameters: "--install-wt-profile --quiet"; StatusMsg: "Adding Windows Terminal profile..."; Flags: runhidden waituntilterminated; Tasks: wtprofile
; niubash#211: after the profile lands, upgrade the shortcut files to the WT
; form (wt.exe resolved at runtime: WindowsApps alias -> PATH -> registry).
; --update-only so a declined desktopicon task never grows a desktop icon.
Filename: "{app}\{#AppExeName}"; Parameters: "--install-wt-shortcuts --quiet --update-only --desktop ""{autodesktop}"" --start-menu ""{group}"""; StatusMsg: "Applying Windows Terminal shortcuts..."; Flags: runhidden waituntilterminated; Tasks: wtprofile

[Code]
const
  NiubashEnvironmentKey = 'Environment';
  NiubashPathValueName = 'Path';
  NiubashHwndBroadcast = $FFFF;
  NiubashSettingChangeMessage = $001A;
  NiubashSendMessageAbortIfHung = $0002;

function SendMessageTimeout(hWnd: Longint; Msg: Longint; wParam: Longint; lParam: string; fuFlags: Longint; uTimeout: Longint; var lpdwResult: Longint): Longint;
  external 'SendMessageTimeoutW@user32.dll stdcall';
function TrimPathSeparators(Value: string): string;
begin
  Result := Value;
  while (Length(Result) > 3) and
    ((Result[Length(Result)] = '\') or (Result[Length(Result)] = '/')) do
  begin
    Delete(Result, Length(Result), 1);
  end;
end;

function PathContainsDir(PathValue: string; Dir: string): Boolean;
begin
  Result :=
    Pos(
      ';' + Uppercase(TrimPathSeparators(Dir)) + ';',
      ';' + Uppercase(TrimPathSeparators(PathValue)) + ';'
    ) > 0;
end;

procedure BroadcastEnvironmentChange();
var
  ResultCode: Longint;
begin
  SendMessageTimeout(
    NiubashHwndBroadcast,
    NiubashSettingChangeMessage,
    0,
    'Environment',
    NiubashSendMessageAbortIfHung,
    5000,
    ResultCode
  );
end;

procedure AddAppDirToUserPath();
var
  PathValue: string;
  AppDir: string;
begin
  AppDir := ExpandConstant('{app}');
  if not RegQueryStringValue(HKEY_CURRENT_USER, NiubashEnvironmentKey, NiubashPathValueName, PathValue) then
  begin
    PathValue := '';
  end;

  if PathContainsDir(PathValue, AppDir) then
  begin
    Exit;
  end;

  if (PathValue <> '') and (PathValue[Length(PathValue)] <> ';') then
  begin
    PathValue := PathValue + ';';
  end;
  PathValue := PathValue + AppDir;
  RegWriteExpandStringValue(HKEY_CURRENT_USER, NiubashEnvironmentKey, NiubashPathValueName, PathValue);
  BroadcastEnvironmentChange();
end;

procedure CloseRunningNiubashForSilentUpdate();
var
  ResultCode: Longint;
begin
  if not WizardSilent then
  begin
    Exit;
  end;

  Exec(
    ExpandConstant('{sys}\taskkill.exe'),
    '/IM niu.exe /F',
    '',
    SW_HIDE,
    ewWaitUntilTerminated,
    ResultCode
  );
end;

procedure RemoveAppDirFromUserPath();
var
  PathValue: string;
  NewValue: string;
  AppDir: string;
  WinuxCmdBinDir: string;
begin
  AppDir := ExpandConstant('{app}');
  // niubash#313 (R1): wpm's install-time `links rebuild` puts
  // {app}\winuxcmd\bin on the user PATH, but the uninstall only removed
  // {app} — every uninstall left that dangling entry behind. Sweep both.
  WinuxCmdBinDir := AppDir + '\winuxcmd\bin';

  if not RegQueryStringValue(HKEY_CURRENT_USER, NiubashEnvironmentKey, NiubashPathValueName, PathValue) then
  begin
    Exit;
  end;

  NewValue := PathValue;
  StringChangeEx(NewValue, WinuxCmdBinDir + ';', '', True);
  StringChangeEx(NewValue, ';' + WinuxCmdBinDir, '', True);
  StringChangeEx(NewValue, AppDir + ';', '', True);
  StringChangeEx(NewValue, ';' + AppDir, '', True);
  if SameText(NewValue, AppDir) or SameText(NewValue, WinuxCmdBinDir) then
  begin
    NewValue := '';
  end;

  if NewValue <> PathValue then
  begin
    RegWriteExpandStringValue(HKEY_CURRENT_USER, NiubashEnvironmentKey, NiubashPathValueName, NewValue);
    BroadcastEnvironmentChange();
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssInstall then
  begin
    CloseRunningNiubashForSilentUpdate();
  end;

  if (CurStep = ssPostInstall) and WizardIsTaskSelected('path') then
  begin
    AddAppDirToUserPath();
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
  begin
    RemoveAppDirFromUserPath();
  end;
end;




