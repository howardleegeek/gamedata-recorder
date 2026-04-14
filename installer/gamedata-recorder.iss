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
; 64-bit architecture support - app is x86_64
ArchitecturesInstallIn64BitMode=x64
ArchitecturesAllowed=x86 x64
; Auto-run after install
SetupIconFile=..\build-resources\owl-logo.ico
; Uninstall entry icon in Add/Remove Programs
UninstallDisplayIcon={app}\{#MyAppExeName}

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "chinese_simplified"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a desktop icon"; GroupDescription: "Additional options:"; Flags: checkedonce
Name: "startup"; Description: "Start automatically when Windows starts"; GroupDescription: "Additional options:"; Flags: checkedonce

[Files]
Source: "..\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion uninsrestartdelete
; OBS DLLs required by libobs
Source: "..\target\release\*.dll"; DestDir: "{app}"; Flags: ignoreversion uninsrestartdelete
; Config is created automatically by the app with defaults if it doesn't exist

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Registry]
; Start with Windows (user-level, no admin needed)
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "{#MyAppName}"; ValueData: """{app}\{#MyAppExeName}"" --minimized"; Flags: uninsdeletevalue; Tasks: startup

[Run]
; Launch after install, minimized to tray
Filename: "{app}\{#MyAppExeName}"; Parameters: "--minimized"; Description: "Launch {#MyAppName}"; Flags: nowait postinstall skipifsilent runasoriginaluser; WorkingDir: "{app}"

[UninstallRun]
; Graceful shutdown attempt first (no /F flag) - wait up to 5 seconds
Filename: "{sys}\taskkill.exe"; Parameters: "/IM {#MyAppExeName}"; Flags: runhidden waituntilterminated; Check: IsAppRunning()
; Brief delay to allow graceful termination to complete (ping trick avoids blocking)
Filename: "{sys}\cmd.exe"; Parameters: "/C ping 127.0.0.1 -n 2 >nul"; Flags: runhidden waituntilterminated; Check: IsAppRunning()
; Force kill if still running after graceful attempt
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM {#MyAppExeName}"; Flags: runhidden waituntilterminated; Check: IsAppRunning()

[Code]
// Check if the application is currently running
function IsAppRunning(): Boolean;
var
  ResultCode: Integer;
  ExecSuccess: Boolean;
begin
  // Query tasklist via cmd to handle PATH/WOW64 issues - returns 0 if found, 1 if not found
  // If tasklist itself fails to execute, assume process is not running to be safe
  // Use {sys} constant for cmd.exe to ensure it's found regardless of PATH
  // Search for the exe name directly (locale-independent) instead of "Running" status
  ExecSuccess := Exec(ExpandConstant('{sys}\cmd.exe'), ExpandConstant('/C "{sys}\tasklist.exe" /FI "IMAGENAME eq {#MyAppExeName}" 2>nul | find "{#MyAppExeName}"'), '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  if not ExecSuccess then
    Result := False
  else
    Result := (ResultCode = 0);
end;

// Prevent install/upgrade if app is already running to avoid file-in-use errors
function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  // Initialize to False - no restart needed for this check
  NeedsRestart := False;
  if IsAppRunning() then
  begin
    Result := 'GameData Recorder is running. Please close it before installing.';
  end else
  begin
    Result := '';
  end;
end;
