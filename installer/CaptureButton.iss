#ifndef AppVersion
  #define AppVersion "0.3.3"
#endif
#ifndef SourceExe
  #define SourceExe "..\target\release\CaptureButton.exe"
#endif
#ifndef ThirdParty
  #define ThirdParty "..\dist\THIRD-PARTY.txt"
#endif

[Setup]
AppId={{A7C3E1B2-4D5F-4A89-9C0E-1B2D3E4F5A60}
AppName=Capture Button
AppVersion={#AppVersion}
AppPublisher=Capture Button
AppPublisherURL=https://github.com/machine202502/capture-button
AppSupportURL=https://github.com/machine202502/capture-button
AppUpdatesURL=https://github.com/machine202502/capture-button/releases
DefaultDirName={autopf}\CaptureButton
DefaultGroupName=CaptureButton
DisableProgramGroupPage=yes
OutputDir=..\dist
OutputBaseFilename=CaptureButton-{#AppVersion}-windows-x64-setup
SetupIconFile=..\assets\CaptureButton.ico
UninstallDisplayIcon={app}\CaptureButton.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=admin
CloseApplications=yes
InfoBeforeFile=..\NOTICE

[Tasks]
Name: desktopicon; Description: "Create a desktop shortcut"; GroupDescription: "Additional icons:"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "CaptureButton.exe"; Flags: ignoreversion
Source: "..\COPYING"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\NOTICE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\locales\*.json"; DestDir: "{app}\locales"; Flags: ignoreversion
Source: "{#ThirdParty}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Capture Button"; Filename: "{app}\CaptureButton.exe"
Name: "{autodesktop}\Capture Button"; Filename: "{app}\CaptureButton.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\CaptureButton.exe"; Description: "Launch Capture Button"; Flags: nowait postinstall skipifsilent
