#ifndef AppVersion
  #error AppVersion must be supplied by build-installer.ps1
#endif
#ifndef AppSource
  #error AppSource must be supplied by build-installer.ps1
#endif
#ifndef InstallerOutput
  #error InstallerOutput must be supplied by build-installer.ps1
#endif

[Setup]
; Keep this ID stable so later versions upgrade the same installation.
AppId={{CE0BC667-7DD2-44F9-A137-50B844307B03}
AppName=Tailout
AppVersion={#AppVersion}
AppVerName=Tailout {#AppVersion}
AppComments=Video trimming and HEVC batch conversion with FFmpeg
VersionInfoDescription=Tailout Setup
VersionInfoVersion={#AppVersion}
DefaultDirName={localappdata}\Programs\Tailout
DefaultGroupName=Tailout
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
WizardStyle=modern
DisableProgramGroupPage=yes
DisableWelcomePage=no
InfoBeforeFile=requirements.txt
SetupIconFile=..\assets\tailout.ico
UninstallDisplayIcon={app}\tailout.exe
OutputDir={#InstallerOutput}
OutputBaseFilename=Tailout-{#AppVersion}-windows-x64-setup
Compression=lzma2
SolidCompression=yes
SetupLogging=yes
; Never close an active export automatically, including in silent mode.
CloseApplications=no
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked

[Files]
Source: "{#AppSource}"; DestDir: "{app}"; DestName: "tailout.exe"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "requirements.txt"; DestDir: "{app}"; DestName: "Requirements.txt"; Flags: ignoreversion

[Icons]
Name: "{group}\Tailout"; Filename: "{app}\tailout.exe"; WorkingDir: "{app}"
Name: "{autodesktop}\Tailout"; Filename: "{app}\tailout.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\tailout.exe"; Description: "Launch Tailout"; Flags: nowait postinstall skipifsilent

; No wildcard deletion: uninstall leaves preferences, presets, videos and any
; user-supplied FFmpeg binaries untouched.
