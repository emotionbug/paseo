Unicode true
!include "MUI2.nsh"
!include "${INVENTORY_INCLUDE}"
Name "Paseo Sessions"
OutFile "${OUTPUT}"
InstallDir "$LOCALAPPDATA\Programs\Paseo Sessions"
InstallDirRegKey HKCU "Software\Paseo Sessions" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
Icon "${ICON}"
UninstallIcon "${ICON}"
VIProductVersion "${NUMERIC_VERSION}"
VIAddVersionKey /LANG=1033 "ProductName" "Paseo Sessions"
VIAddVersionKey /LANG=1033 "FileDescription" "Paseo Sessions Flutter Installer"
VIAddVersionKey /LANG=1033 "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=1033 "LegalCopyright" "Paseo contributors"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

; Refuse collisions with files not owned by an existing installation at this
; exact location. A shared user-selected directory is never implicitly owned.
Function PaseoCheckOwnedFile
  Pop $0
  ReadRegStr $3 HKCU "Software\Paseo Sessions" "InstallDir"
  StrCmp $3 $INSTDIR 0 unrelated
  ClearErrors
  FileOpen $1 "$INSTDIR\.paseo-sessions-owned-files" r
  IfErrors unrelated
  inventory_loop:
    ClearErrors
    FileRead $1 $2
    IfErrors inventory_missing
    StrCmp $2 "$0$\r$\n" owned
    StrCmp $2 "$0$\n" owned inventory_loop
  inventory_missing:
    FileClose $1
    Goto unrelated
  owned:
    FileClose $1
    Return
  unrelated:
    MessageBox MB_OK|MB_ICONSTOP "The selected folder contains a file not owned by Paseo Sessions:$\r$\n$0$\r$\nChoose another installation folder. No package files have been written."
    Abort
FunctionEnd

Section "Paseo Sessions" SEC_MAIN
  SetShellVarContext current
  !insertmacro PaseoCheckOwnedFiles
  SetOutPath "$INSTDIR"
  File /r "${SOURCE}\*"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  File /oname=.paseo-sessions-owned-files "${INVENTORY_MANIFEST}"
  WriteRegStr HKCU "Software\Paseo Sessions" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Classes\paseo" "" "URL:Paseo agent link"
  WriteRegStr HKCU "Software\Classes\paseo" "URL Protocol" ""
  WriteRegStr HKCU "Software\Classes\paseo\DefaultIcon" "" '$"$INSTDIR\paseo_sessions.exe$",0'
  WriteRegStr HKCU "Software\Classes\AppUserModelId\sh.paseo.sessions.desktop" "DisplayName" "Paseo Sessions"
  WriteRegStr HKCU "Software\Classes\AppUserModelId\sh.paseo.sessions.desktop" "IconUri" "$INSTDIR\paseo_sessions.exe"
  WriteRegStr HKCU "Software\Classes\paseo\shell\open\command" "" '$"$INSTDIR\paseo_sessions.exe$" $"%1$"'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\sh.paseo.sessions.desktop" "DisplayName" "Paseo Sessions"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\sh.paseo.sessions.desktop" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\sh.paseo.sessions.desktop" "UninstallString" '$"$INSTDIR\Uninstall.exe$"'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\sh.paseo.sessions.desktop" "InstallLocation" "$INSTDIR"
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\sh.paseo.sessions.desktop" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\sh.paseo.sessions.desktop" "NoRepair" 1
  CreateShortCut "$SMPROGRAMS\Paseo Sessions.lnk" "$INSTDIR\paseo_sessions.exe"
  CreateShortCut "$DESKTOP\Paseo Sessions.lnk" "$INSTDIR\paseo_sessions.exe"
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  Delete "$SMPROGRAMS\Paseo Sessions.lnk"
  Delete "$DESKTOP\Paseo Sessions.lnk"
  ReadRegStr $0 HKCU "Software\Classes\paseo\shell\open\command" ""
  StrCmp $0 '$"$INSTDIR\paseo_sessions.exe$" $"%1$"' 0 +2
  DeleteRegKey HKCU "Software\Classes\paseo"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\sh.paseo.sessions.desktop"
  DeleteRegKey HKCU "Software\Paseo Sessions"
  !insertmacro PaseoRemoveOwnedFiles
  ; Daemon configuration, credentials, sessions and user CLI links are never deleted here.
SectionEnd
