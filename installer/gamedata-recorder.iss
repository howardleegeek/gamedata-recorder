; GameData Recorder Installer - Inno Setup Script
; Builds a one-click Windows installer

#define MyAppName "GameData Recorder"
#define MyAppVersion "1.6.1"
#define MyAppPublisher "GameData Labs"
#define MyAppURL "https://gamedatalabs.com"
#define MyAppExeName "gamedata-recorder.exe"

[Setup]
AppId={{A1B2C3D4-E5F6-7890-ABCD-EF1234567890}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
OutputDir=.\output
OutputBaseFilename=GameDataRecorder-Setup-{#MyAppVersion}
Compression=lzma2/ultra
SolidCompression=yes
; No admin required for per-user install
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
; Modern UI
WizardStyle=modern
; Auto-run after install
SetupIconFile=..\assets\icon_idle.ico

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "chinese_simplified"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a desktop icon"; GroupDescription: "Additional options:"; Flags: checkedonce
Name: "startup"; Description: "Start automatically when Windows starts"; GroupDescription: "Additional options:"; Flags: checkedonce

[Files]
Source: "..\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
; OBS DLLs required by libobs
Source: "..\target\release\*.dll"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs
; Default config
Source: "..\config.default.toml"; DestDir: "{app}"; DestName: "config.toml"; Flags: onlyifdoesntexist

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Registry]
; Start with Windows (user-level, no admin needed)
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "{#MyAppName}"; ValueData: """{app}\{#MyAppExeName}"" --minimized"; Flags: uninsdeletevalue; Tasks: startup

[Run]
; Launch after install, minimized to tray
Filename: "{app}\{#MyAppExeName}"; Parameters: "--minimized"; Description: "Launch {#MyAppName}"; Flags: nowait postinstall skipifsilent runasoriginaluser

[UninstallRun]
; Kill the process before uninstall (only if running to avoid error)
Filename: "taskkill"; Parameters: "/F /IM {#MyAppExeName}"; Flags: runhidden; Check: IsAppRunning()

[Code]
// Check if the application is currently running
function IsAppRunning(): Boolean;
var
  ResultCode: Integer;
  ExecSuccess: Boolean;
begin
  // Query tasklist to check if process exists - returns 0 if found, 1 if not found
  // If tasklist itself fails to execute, assume process is not running to be safe
  ExecSuccess := Exec('tasklist', '/FI "IMAGENAME eq {#MyAppExeName}"', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  if not ExecSuccess then
    Result := False
  else
    Result := (ResultCode = 0);
end;

// Show a simple "Install complete! GameData Recorder will run in your system tray." message
procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    // Nothing extra needed - the [Run] section handles auto-launch
  end;
end;
