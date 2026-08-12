; Spark 安装器（Inno Setup 6）
; 用户级安装（PrivilegesRequired=lowest）：默认 %LOCALAPPDATA%\Programs\Spark，
; 向导支持自定义路径；静默更新传 /DIR= 原地覆盖，全程不弹 UAC。
; 编译：scripts\build_installer.ps1（ISCC /DAppVersion=… /DSourceDir=…）

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef SourceDir
  #define SourceDir "dist"
#endif

#define MyAppId "D9260DBB-4338-4B63-A27D-A9D3947C34F2"
#define MyAppName "Spark"
#define MyAppHostExe "spark-host.exe"

[Setup]
AppId={#MyAppId}
AppName={#MyAppName}
AppVersion={#AppVersion}
AppPublisher=Spark
AppPublisherURL=https://github.com/MrHan-Yd/spark
DefaultDirName={userpf}\Spark
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=output
OutputBaseFilename=Spark-{#AppVersion}-setup
SetupIconFile=..\ui\Spark.UI\Assets\spark.ico
Compression=lzma2/ultra
SolidCompression=yes
WizardStyle=modern
; 更新时强制关闭运行中的 Spark.exe / spark-host.exe（解决运行时替换文件）
CloseApplications=force
CloseApplicationsFilter=Spark.exe,spark-host.exe
AppMutex=SparkUISingleInstance_v1,SparkLauncherHost_v1
; 静默安装日志（%TEMP%\Setup Log*.txt），排查更新失败用
SetupLogging=yes
UninstallDisplayIcon={app}\Spark.exe
VersionInfoVersion={#AppVersion}.0
ArchitecturesAllowed=x64compatible

[Tasks]
Name: desktopicon; Description: 创建桌面快捷方式; GroupDescription: 附加图标

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs; Excludes: "*.pdb"

[Registry]
Root: HKCU; Subkey: "Software\Spark"; ValueType: string; ValueName: "InstallPath"; ValueData: "{app}"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Spark"; ValueType: string; ValueName: "Version"; ValueData: "{#AppVersion}"; Flags: uninsdeletevalue

[Icons]
Name: "{autoprograms}\Spark"; Filename: "{app}\{#MyAppHostExe}"
Name: "{autodesktop}\Spark"; Filename: "{app}\{#MyAppHostExe}"; Tasks: desktopicon

[Run]
; 装完拉起 host（host 会自行拉起 UI）；静默更新同样生效，完成"自动重启"闭环
Filename: "{app}\{#MyAppHostExe}"; Description: 启动 Spark; Flags: nowait

[UninstallRun]
; 卸载前先结束常驻进程，避免文件占用导致残留
Filename: "{sys}\taskkill.exe"; Parameters: "/IM Spark.exe /F"; Flags: runhidden
Filename: "{sys}\taskkill.exe"; Parameters: "/IM spark-host.exe /F"; Flags: runhidden
