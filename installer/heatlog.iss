; Inno Setup script for heatlog. Per-user install under %LOCALAPPDATA%, so
; no admin prompt is needed and the in-app updater can replace the files.
; Build:  ISCC.exe /DAppVersion=0.2.0 installer\heatlog.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{4C9F5B2E-18D7-4C44-9A3B-7F1A6E2D5C10}
AppName=heatlog
AppVersion={#AppVersion}
AppPublisher=Hami Khatri
AppPublisherURL=https://github.com/Hami-K/heatlog
AppSupportURL=https://github.com/Hami-K/heatlog/issues
DefaultDirName={localappdata}\Programs\heatlog
DefaultGroupName=heatlog
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=dist
OutputBaseFilename=heatlog-setup
SetupIconFile=heatlog.ico
UninstallDisplayIcon={app}\heatlog.ico
UninstallDisplayName=heatlog {#AppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; Close a running heatlog so its files can be replaced during an update.
CloseApplications=yes
RestartApplications=no

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Files]
Source: "..\target\release\heatlog-gui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\heatlog.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "heatlog.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion

; AppUserModelID matches the id heatlog sets on its own window, so
; Windows treats a pinned shortcut and the running app as one app - which
; is what keeps a taskbar pin working after an in-app update.
[Icons]
Name: "{autoprograms}\heatlog"; Filename: "{app}\heatlog-gui.exe"; IconFilename: "{app}\heatlog.ico"; AppUserModelID: "HamiKhatri.heatlog"
Name: "{autodesktop}\heatlog"; Filename: "{app}\heatlog-gui.exe"; IconFilename: "{app}\heatlog.ico"; AppUserModelID: "HamiKhatri.heatlog"; Tasks: desktopicon

[Run]
Filename: "{app}\heatlog-gui.exe"; Description: "Start heatlog"; Flags: nowait postinstall skipifsilent
