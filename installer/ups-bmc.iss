; UPS-BMC installer (Inno Setup 6). Build with installer\build.ps1.
;
; Per-user install: no administrator rights; files go to
; %LOCALAPPDATA%\Programs\UPS-BMC. The database and logs in
; %LOCALAPPDATA%\UPS-BMC are user data and are kept on uninstall.

#ifndef AppVersion
  #define AppVersion "0.0.0-dev"
#endif
#define AppExe "ups-bmc.exe"
; Must match APP_USER_MODEL_ID in apps/ups-bmc/src/main.rs, so Windows
; notifications show "UPS-BMC" as their sender.
#define AppUserModelId "UPS-BMC.Desktop"
; Must match autostart::VALUE_NAME in apps/ups-bmc/src/autostart.rs.
#define RunValue "UPS-BMC"

[Setup]
; Never change AppId: it identifies the installation for upgrades.
AppId={{E75643C4-5C89-4DF7-8010-07E1E0A9B2D7}
AppName=UPS-BMC
AppVersion={#AppVersion}
AppVerName=UPS-BMC {#AppVersion}
AppPublisher=Alexander Rubtsov
AppPublisherURL=https://github.com/Alexxfromgit/UPS-BMC
AppSupportURL=https://github.com/Alexxfromgit/UPS-BMC/issues
AppComments=UPS & Battery Monitoring Center
DefaultDirName={autopf}\UPS-BMC
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir=..\target\installer
OutputBaseFilename=UPS-BMC-setup-{#AppVersion}
SetupIconFile=..\apps\ups-bmc\ui\icons\ups-bmc.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName=UPS-BMC
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=no

[Tasks]
Name: "autostart"; Description: "Start UPS-BMC when I sign in (keeps monitoring running)"; GroupDescription: "Background monitoring:"

[Files]
Source: "..\target\release\ups-bmc.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\ups-collector.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\hid-probe.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\UPS-BMC"; Filename: "{app}\{#AppExe}"; Comment: "UPS & Battery Monitoring Center"; AppUserModelID: "{#AppUserModelId}"

[Registry]
; Autostart, if chosen. The same value is toggled by Settings > Start with Windows.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "{#RunValue}"; ValueData: """{app}\{#AppExe}"" --minimized"; Tasks: autostart
; Always remove the value on uninstall (it may have been enabled later in the app).
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "{#RunValue}"; Flags: uninsdeletevalue

[Run]
Filename: "{app}\{#AppExe}"; Description: "Start UPS-BMC now"; Flags: nowait postinstall skipifsilent
; Silent installs (scripted upgrades) have no "start now" page: restart
; monitoring in the notification area anyway, since the upgrade stopped it.
Filename: "{app}\{#AppExe}"; Parameters: "--minimized"; Flags: nowait; Check: WizardSilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM ups-bmc.exe"; Flags: runhidden; RunOnceId: "StopUi"
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM ups-collector.exe"; Flags: runhidden; RunOnceId: "StopCollector"

[Code]
// Upgrades replace the executables, so stop the running UI and collector
// first. The collector has no window, so the Restart Manager can't close it.
// Ask it to stop gracefully first (it then records "Collector stopped" and
// saves pending data), and force it only if it is still running.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM ups-bmc.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  if FileExists(ExpandConstant('{sys}\curl.exe')) then
  begin
    Exec(ExpandConstant('{sys}\curl.exe'),
      '-s -m 3 -X POST -H "Content-Type: application/json" -d "{}" http://127.0.0.1:47800/api/v1/shutdown',
      '', SW_HIDE, ewWaitUntilTerminated, Code);
    if Code = 0 then
      Sleep(4000);
  end;
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM ups-collector.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Result := '';
end;
