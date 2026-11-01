; Per-user installer: no admin prompt, nothing outside this user's profile.
; Build with: makensis /DVERSION=x.y.z installer\rawr.nsi
Unicode true
!include "MUI2.nsh"

Name "rawr"
OutFile "..\target\release\rawr-${VERSION}-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\rawr"
InstallDirRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\rawr" "InstallLocation"
RequestExecutionLevel user
SetCompressor /SOLID lzma

!define MUI_ICON "..\rawr.ico"
!define MUI_UNICON "..\rawr.ico"
!define MUI_FINISHPAGE_RUN "$INSTDIR\rawr.exe"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

; Replacing a running exe fails, so make sure it's closed first.
!macro RequireClosed
  IfFileExists "$INSTDIR\rawr.exe" 0 +7
  ClearErrors
  FileOpen $0 "$INSTDIR\rawr.exe" a
  IfErrors 0 +3
  MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "rawr is running. Close it, then choose Retry." /SD IDCANCEL IDRETRY -3
  Abort
  FileClose $0
!macroend

Section
  !insertmacro RequireClosed
  SetOutPath "$INSTDIR"
  File "..\target\release\rawr.exe"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateShortcut "$SMPROGRAMS\rawr.lnk" "$INSTDIR\rawr.exe"
  ; Lists rawr in Open with and Default apps. The user's default stays theirs.
  ExecWait '"$INSTDIR\rawr.exe" --register'
  !define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\rawr"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "rawr"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\rawr.exe"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  !insertmacro RequireClosed
  ; Takes back Explorer menus, the .rar association and settings.
  ExecWait '"$INSTDIR\rawr.exe" --remove-settings'
  Delete "$SMPROGRAMS\rawr.lnk"
  Delete "$INSTDIR\rawr.exe"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\rawr"
SectionEnd
