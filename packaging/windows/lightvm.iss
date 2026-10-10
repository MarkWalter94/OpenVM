; Experimental per-user installer of LightVM (ROADMAP F2), Inno Setup 6.
; Compiled by build-package.ps1 from the staged package (same files as the
; zip). No administrator rights (PrivilegesRequired=lowest): installs into
; %LOCALAPPDATA%\Programs\LightVM, Start Menu shortcut for the current user,
; no PATH change. The uninstaller removes only the installed files: the VM
; catalog and the settings in %LOCALAPPDATA%\LightVM are never touched.
; NOT code-signed (ADR-0020). QEMU is not included (ADR-0010).

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef AppNumericVersion
  #define AppNumericVersion "0.0.0"
#endif
#ifndef StagingDir
  #error StagingDir must be defined (/DStagingDir=...)
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif
#ifndef OutputBaseName
  #define OutputBaseName "LightVM-setup"
#endif

[Setup]
AppId={{6C1E7C52-2B7B-4C55-9E4E-6A1D3F0B8E21}
AppName=LightVM
AppVersion={#AppVersion}
AppVerName=LightVM {#AppVersion} (experimental alpha)
VersionInfoVersion={#AppNumericVersion}
AppPublisher=LightVM project
DefaultDirName={localappdata}\Programs\LightVM
DefaultGroupName=LightVM
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.22000
ChangesEnvironment=no
OutputDir={#OutputDir}
OutputBaseFilename={#OutputBaseName}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
LicenseFile={#StagingDir}\LICENSE.txt
UninstallDisplayName=LightVM {#AppVersion} (experimental)
UninstallDisplayIcon={app}\lightvm-desktop.exe
CloseApplications=no

[Files]
Source: "{#StagingDir}\lightvm-desktop.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StagingDir}\lightvm-service.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StagingDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StagingDir}\USER_GUIDE.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StagingDir}\LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StagingDir}\THIRD-PARTY-LICENSES.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StagingDir}\BUILDINFO.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StagingDir}\SHA256SUMS.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\LightVM"; Filename: "{app}\lightvm-desktop.exe"; WorkingDir: "{app}"; Comment: "LightVM (experimental alpha)"

[Run]
Filename: "{app}\USER_GUIDE.md"; Description: "Open the user guide"; Flags: postinstall shellexec skipifsilent unchecked
