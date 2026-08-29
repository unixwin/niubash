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

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\Niubash"; Filename: "{app}\{#AppExeName}"; WorkingDir: "{%USERPROFILE}"; IconFilename: "{app}\assets\niubash-icon.ico"

[Run]
Filename: "{app}\winuxcmd\usr\bin\winuxcmd.exe"; Parameters: "wpm links rebuild --root ""{app}\winuxcmd"" --force"; WorkingDir: "{app}\winuxcmd\usr\bin"; StatusMsg: "Activating WinuxCmd command links..."; Flags: runhidden waituntilterminated
Filename: "{app}\{#AppExeName}"; Parameters: "--install-wt-profile --quiet"; StatusMsg: "Adding Windows Terminal profile..."; Flags: runhidden waituntilterminated; Tasks: wtprofile

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
  AppDir: string;
begin
  AppDir := ExpandConstant('{app}');
  if not RegQueryStringValue(HKEY_CURRENT_USER, NiubashEnvironmentKey, NiubashPathValueName, PathValue) then
  begin
    Exit;
  end;

  StringChangeEx(PathValue, AppDir + ';', '', True);
  StringChangeEx(PathValue, ';' + AppDir, '', True);
  if SameText(PathValue, AppDir) then
  begin
    PathValue := '';
  end;

  RegWriteExpandStringValue(HKEY_CURRENT_USER, NiubashEnvironmentKey, NiubashPathValueName, PathValue);
  BroadcastEnvironmentChange();
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




