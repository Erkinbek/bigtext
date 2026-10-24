; Inno Setup script for the optional BigText installer.
; Build: ISCC.exe /DSourceExe=path\to\bigtext.exe installer\bigtext.iss
; The portable bigtext.exe works without installing; the installer adds Start menu
; shortcuts and "Open with" / Explorer context-menu entries.

#ifndef SourceExe
  #define SourceExe "..\target\release\bigtext.exe"
#endif
#define AppName "BigText"
#define AppVersion "0.1.0"

[Setup]
AppId={{7B0C2A51-3E2B-4C55-9C3A-6B3F8E1D2B10}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=BigText
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
OutputBaseFilename=bigtext-{#AppVersion}-setup
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequiredOverridesAllowed=dialog
ChangesAssociations=yes
UninstallDisplayIcon={app}\bigtext.exe

[Tasks]
Name: "desktopicon"; Description: "Create a desktop icon"; Flags: unchecked
Name: "contextmenu"; Description: "Add ""Open with BigText"" to the Explorer context menu for all files"

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "bigtext.exe"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\bigtext.exe"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\bigtext.exe"; Tasks: desktopicon

[Registry]
; "Open with" support
Root: HKA; Subkey: "Software\Classes\Applications\bigtext.exe"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "{#AppName}"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Applications\bigtext.exe\shell\open\command"; ValueType: string; ValueData: """{app}\bigtext.exe"" ""%1"""
Root: HKA; Subkey: "Software\Classes\Applications\bigtext.exe\SupportedTypes"; ValueType: string; ValueName: ".txt"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\Applications\bigtext.exe\SupportedTypes"; ValueType: string; ValueName: ".log"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\Applications\bigtext.exe\SupportedTypes"; ValueType: string; ValueName: ".csv"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\Applications\bigtext.exe\SupportedTypes"; ValueType: string; ValueName: ".sql"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\Applications\bigtext.exe\SupportedTypes"; ValueType: string; ValueName: ".json"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.txt\OpenWithProgids"; ValueType: string; ValueName: "Applications\bigtext.exe"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\.log\OpenWithProgids"; ValueType: string; ValueName: "Applications\bigtext.exe"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\.csv\OpenWithProgids"; ValueType: string; ValueName: "Applications\bigtext.exe"; ValueData: ""; Flags: uninsdeletevalue
; Explorer context menu for any file
Root: HKA; Subkey: "Software\Classes\*\shell\BigText"; ValueType: string; ValueData: "Open with BigText"; Tasks: contextmenu; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\*\shell\BigText"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\bigtext.exe"; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\*\shell\BigText\command"; ValueType: string; ValueData: """{app}\bigtext.exe"" ""%1"""; Tasks: contextmenu

[Run]
Filename: "{app}\bigtext.exe"; Description: "Launch {#AppName}"; Flags: nowait postinstall skipifsilent
