; Script generato per RustClip
; Richiede Inno Setup

#define MyAppName "RustClip"
#define MyAppVersion "1.0"
#define MyAppPublisher "RustClip Team"
#define MyAppExeName "rust-clip.exe"

[Setup]
; ID Univoco dell'app (generato casualmente, non cambiarlo dopo il primo rilascio)
AppId={{A1B2C3D4-E5F6-7890-1234-567890ABCDEF}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
DefaultDirName={autopf}\{#MyAppName}
DisableProgramGroupPage=yes
; Richiedi permessi di Amministratore (serve per Firewall e Program Files)
PrivilegesRequired=admin
OutputDir=target\installer
OutputBaseFilename=RustClipSetup
Compression=lzma
SolidCompression=yes
WizardStyle=modern
; Icona dell'installer (usa la tua icona se è .ico, altrimenti commenta questa riga)
SetupIconFile=assets\icon.ico 

[Languages]
Name: "italian"; MessagesFile: "compiler:Languages\Italian.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "autostart"; Description: "Avvia RustClip all'accensione di Windows"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
; PRENDE L'ESEGUIBILE COMPILATO DA RUST
; Nota: Assicurati di aver fatto 'cargo build --release' prima!
Source: "target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; Aggiungi l'indice dell'icona (0 è l'icona principale embeddata nel file)
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\{#MyAppExeName}"; IconIndex: 0
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon; IconFilename: "{app}\{#MyAppExeName}"; IconIndex: 0

[Run]
; 1. REGOLA FIREWALL (Aggiunge eccezione TCP 5566 in entrata)
Filename: "netsh"; Parameters: "advfirewall firewall add rule name=""RustClip TCP"" dir=in action=allow protocol=TCP localport=5566"; Flags: runhidden; StatusMsg: "Configurazione Firewall in corso..."

; 2. AVVIO AUTOMATICO (Opzionale, aggiunge chiave di registro se l'utente ha spuntato il task)
Filename: "reg"; Parameters: "add ""HKCU\Software\Microsoft\Windows\CurrentVersion\Run"" /v ""{#MyAppName}"" /t REG_SZ /d ""\""{app}\{#MyAppExeName}\"""" /f"; Flags: runhidden; Tasks: autostart

; 3. LANCIA L'APP ALLA FINE
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; RIMUOVE REGOLA FIREWALL ALLA DISINSTALLAZIONE
Filename: "netsh"; Parameters: "advfirewall firewall delete rule name=""RustClip TCP"""; Flags: runhidden
; Rimuove autostart
Filename: "reg"; Parameters: "delete ""HKCU\Software\Microsoft\Windows\CurrentVersion\Run"" /v ""{#MyAppName}"" /f"; Flags: runhidden