#define AppVersion GetEnv("RUSTFRP_VERSION")
[Setup]
AppId={{F04B1184-00D2-4537-A385-C3A4F10569C1}
AppName=RustFRP
AppVersion={#AppVersion}
AppPublisher=RustFRP contributors
DefaultDirName={autopf}\RustFRP
DefaultGroupName=RustFRP
OutputDir=dist
OutputBaseFilename=rustfrp-windows-x86_64-setup
ArchitecturesInstallIn64BitMode=x64
Compression=lzma2
SolidCompression=yes
PrivilegesRequired=lowest
WizardStyle=modern
CloseApplications=yes
CloseApplicationsFilter=rustfrp_app.exe
RestartApplications=no
UninstallDisplayIcon={app}\rustfrp_app.exe

[Files]
Source: "dist\windows\*"; DestDir: "{app}"; Flags: recursesubdirs ignoreversion

[Icons]
Name: "{group}\RustFRP"; Filename: "{app}\rustfrp_app.exe"
Name: "{autodesktop}\RustFRP"; Filename: "{app}\rustfrp_app.exe"

[Run]
Filename: "{app}\rustfrp_app.exe"; Description: "Launch RustFRP"; Flags: postinstall nowait
