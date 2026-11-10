; Blanket installer (Inno Setup 6). Built by scripts\package.ps1, which passes:
;   /DAppVersion=0.1.0  /DSourceDir=<staged package folder>  /DIconFile=<blanket.ico>
;
; - Installs for the current user without admin rights (or for everyone, if chosen).
; - Start menu and desktop shortcuts, and registration as a web browser so Blanket can be
;   picked in Windows "Default apps".
; - Upgrades in place (closing a running Blanket), checks for the WebView2 runtime used by
;   compatibility mode, and the uninstaller offers to remove browsing data.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #error SourceDir must point at the staged package folder
#endif

#define AppName "Blanket"
#define AppExe "blanket.exe"
#define ProgId "BlanketHTML"
#define ClientKey "Software\Clients\StartMenuInternet\Blanket"

[Setup]
AppId={{6F0C1E3A-8B7D-4C2E-9A51-B1A7E7D0C0DE}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=CaduhRoman
AppPublisherURL=https://github.com/CaduhRoman/blanket
AppSupportURL=https://github.com/CaduhRoman/blanket/issues
AppUpdatesURL=https://github.com/CaduhRoman/blanket/releases
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputBaseFilename={#AppName}-{#AppVersion}-setup
#ifdef IconFile
SetupIconFile={#IconFile}
#endif
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
WizardStyle=modern
Compression=lzma2/ultra64
SolidCompression=yes
CloseApplications=yes
RestartApplications=no
ChangesAssociations=yes
LicenseFile={#SourceDir}\LICENSE

[Languages]
Name: "ptbr"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"
Name: "en"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
ptbr.DesktopIcon=Criar um atalho na Área de Trabalho
ptbr.RegisterBrowser=Registrar o Blanket como navegador (para poder torná-lo o padrão)
ptbr.OpenDefaults=Abrir as configurações para tornar o Blanket o navegador padrão
ptbr.Launch=Abrir o Blanket agora
ptbr.NoWebView2=O Microsoft Edge WebView2 Runtime não foi encontrado.%n%nEle é usado pelo modo de compatibilidade (YouTube, Twitch, streaming). O Blanket funciona sem ele, mas esses sites não vão tocar vídeo.%n%nAbrir a página de download agora?
ptbr.RemoveData=Remover também seus dados de navegação (histórico, favoritos, sessão, cookies)?
ptbr.Description=Navegador leve, escrito em Rust
en.DesktopIcon=Create a desktop shortcut
en.RegisterBrowser=Register Blanket as a web browser (so it can be made the default)
en.OpenDefaults=Open settings to make Blanket the default browser
en.Launch=Launch Blanket now
en.NoWebView2=The Microsoft Edge WebView2 Runtime was not found.%n%nCompatibility mode (YouTube, Twitch, streaming) uses it. Blanket works without it, but those sites will not play video.%n%nOpen the download page now?
en.RemoveData=Also remove your browsing data (history, bookmarks, session, cookies)?
en.Description=A lightweight browser written in Rust

[Tasks]
Name: "desktopicon"; Description: "{cm:DesktopIcon}"
Name: "registerbrowser"; Description: "{cm:RegisterBrowser}"

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"; Comment: "{cm:Description}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Comment: "{cm:Description}"; Tasks: desktopicon

[Registry]
; Document type opened by Blanket.
Root: HKA; Subkey: "Software\Classes\{#ProgId}"; ValueType: string; ValueData: "Blanket HTML Document"; Flags: uninsdeletekey; Tasks: registerbrowser
Root: HKA; Subkey: "Software\Classes\{#ProgId}\DefaultIcon"; ValueType: string; ValueData: "{app}\{#AppExe},0"; Tasks: registerbrowser
Root: HKA; Subkey: "Software\Classes\{#ProgId}\shell\open\command"; ValueType: string; ValueData: """{app}\{#AppExe}"" ""%1"""; Tasks: registerbrowser
; Browser registration (Default apps).
Root: HKA; Subkey: "{#ClientKey}"; ValueType: string; ValueData: "{#AppName}"; Flags: uninsdeletekey; Tasks: registerbrowser
Root: HKA; Subkey: "{#ClientKey}\DefaultIcon"; ValueType: string; ValueData: "{app}\{#AppExe},0"; Tasks: registerbrowser
Root: HKA; Subkey: "{#ClientKey}\shell\open\command"; ValueType: string; ValueData: """{app}\{#AppExe}"""; Tasks: registerbrowser
Root: HKA; Subkey: "{#ClientKey}\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "{#AppName}"; Tasks: registerbrowser
Root: HKA; Subkey: "{#ClientKey}\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "{cm:Description}"; Tasks: registerbrowser
Root: HKA; Subkey: "{#ClientKey}\Capabilities"; ValueType: string; ValueName: "ApplicationIcon"; ValueData: "{app}\{#AppExe},0"; Tasks: registerbrowser
Root: HKA; Subkey: "{#ClientKey}\Capabilities\FileAssociations"; ValueType: string; ValueName: ".htm"; ValueData: "{#ProgId}"; Tasks: registerbrowser
Root: HKA; Subkey: "{#ClientKey}\Capabilities\FileAssociations"; ValueType: string; ValueName: ".html"; ValueData: "{#ProgId}"; Tasks: registerbrowser
Root: HKA; Subkey: "{#ClientKey}\Capabilities\URLAssociations"; ValueType: string; ValueName: "http"; ValueData: "{#ProgId}"; Tasks: registerbrowser
Root: HKA; Subkey: "{#ClientKey}\Capabilities\URLAssociations"; ValueType: string; ValueName: "https"; ValueData: "{#ProgId}"; Tasks: registerbrowser
Root: HKA; Subkey: "{#ClientKey}\Capabilities\StartMenu"; ValueType: string; ValueName: "StartMenuInternet"; ValueData: "Blanket"; Tasks: registerbrowser
Root: HKA; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "{#AppName}"; ValueData: "{#ClientKey}\Capabilities"; Flags: uninsdeletevalue; Tasks: registerbrowser
; "Open with" for web pages.
Root: HKA; Subkey: "Software\Classes\.html\OpenWithProgids"; ValueType: string; ValueName: "{#ProgId}"; ValueData: ""; Flags: uninsdeletevalue; Tasks: registerbrowser
Root: HKA; Subkey: "Software\Classes\.htm\OpenWithProgids"; ValueType: string; ValueName: "{#ProgId}"; ValueData: ""; Flags: uninsdeletevalue; Tasks: registerbrowser

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:Launch}"; Flags: nowait postinstall skipifsilent
Filename: "ms-settings:defaultapps"; Description: "{cm:OpenDefaults}"; Flags: shellexec nowait postinstall skipifsilent unchecked; Tasks: registerbrowser

[Code]
function WebView2Installed(): Boolean;
var
  Version: String;
begin
  Result :=
    RegQueryStringValue(HKLM, 'SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}', 'pv', Version) or
    RegQueryStringValue(HKCU, 'Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}', 'pv', Version) or
    RegQueryStringValue(HKLM, 'SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}', 'pv', Version);
  Result := Result and (Version <> '') and (Version <> '0.0.0.0');
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  ErrorCode: Integer;
begin
  if (CurStep = ssPostInstall) and not WizardSilent() and not WebView2Installed() then
    if MsgBox(CustomMessage('NoWebView2'), mbConfirmation, MB_YESNO) = IDYES then
      ShellExec('open', 'https://go.microsoft.com/fwlink/p/?LinkId=2124703', '', '', SW_SHOWNORMAL, ewNoWait, ErrorCode);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if (CurUninstallStep = usPostUninstall) and not UninstallSilent() then
    if MsgBox(CustomMessage('RemoveData'), mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
      DelTree(ExpandConstant('{userappdata}\Blanket'), True, True, True);
end;
