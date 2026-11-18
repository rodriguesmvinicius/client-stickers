; Windows installer for the oxidezap client with favorite stickers.
;
; Built with NSIS 3 (`makensis`), on Windows or from Linux:
;
;   makensis -DVERSION=0.1.0-stickers -DSTAGE=path/to/binaries \
;            -DOUTFILE=oxidezap-setup.exe packaging/windows/installer.nsi
;
; STAGE holds oxidezap.exe, oxidezapd.exe, oxidezap-cli.exe and the emoji
; font licence. The install is per user: no administrator prompt, the files
; go under %LOCALAPPDATA%\Programs, and the uninstaller is registered under
; HKCU. The account's data (session, history, media) lives in the user's
; profile, not here, so installing over or uninstalling keeps it.

Unicode true
ManifestDPIAware true
RequestExecutionLevel user
SetCompressor /SOLID lzma

!ifndef VERSION
  !define VERSION "0.1.0"
!endif
!ifndef STAGE
  !error "pass -DSTAGE=<folder with the three binaries>"
!endif
!ifndef OUTFILE
  !define OUTFILE "oxidezap-${VERSION}-windows-x86_64-setup.exe"
!endif

; makensis runs from this script's folder (unless given /NOCD), so paths
; into the repository are relative to it, whatever folder it was called from.
; Backslashes throughout: on Windows `File` splits a path at the last
; backslash only, so "dir/name.exe" is taken as a file named "dir/name.exe"
; and found nowhere. makensis on Linux turns them into slashes itself.
!define ICON "..\..\web\icons\favicon.ico"

!define APPNAME "oxidezap"
!define PUBLISHER "oxidezap (unofficial WhatsApp client)"
!define UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\oxidezap-stickers"

Name "${APPNAME} ${VERSION}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\oxidezap"
InstallDirRegKey HKCU "Software\oxidezap-stickers" "InstallDir"
Icon "${ICON}"
UninstallIcon "${ICON}"

!include "MUI2.nsh"
!include "LogicLib.nsh"

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\oxidezap.exe"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "PortugueseBR"
!insertmacro MUI_LANGUAGE "English"

LangString MsgRunning ${LANG_PORTUGUESEBR} "O oxidezap está em execução (inclusive o serviço da bandeja). Ele precisa ser fechado para continuar.$\r$\n$\r$\nFechar agora? Suas mensagens ficam salvas."
LangString MsgRunning ${LANG_ENGLISH} "oxidezap is running (including its tray service). It must be closed to continue.$\r$\n$\r$\nClose it now? Your messages stay saved."
LangString SecApp ${LANG_PORTUGUESEBR} "oxidezap (obrigatório)"
LangString SecApp ${LANG_ENGLISH} "oxidezap (required)"
LangString SecDesktop ${LANG_PORTUGUESEBR} "Atalho na área de trabalho"
LangString SecDesktop ${LANG_ENGLISH} "Desktop shortcut"

; The window, the daemon and the CLI run from these files; Windows refuses to
; replace an executable that is running, so the daemon has to stop first.
!macro StopRunning
  nsExec::ExecToStack 'cmd /c tasklist /NH /FI "IMAGENAME eq oxidezapd.exe" | find /I "oxidezapd.exe"'
  Pop $0
  Pop $1
  nsExec::ExecToStack 'cmd /c tasklist /NH /FI "IMAGENAME eq oxidezap.exe" | find /I "oxidezap.exe"'
  Pop $2
  Pop $1
  ${If} $0 == 0
  ${OrIf} $2 == 0
    MessageBox MB_YESNO|MB_ICONEXCLAMATION "$(MsgRunning)" IDYES +2
    Abort
    nsExec::Exec 'taskkill /IM oxidezap.exe /F'
    nsExec::Exec 'taskkill /IM oxidezapd.exe /F'
    Sleep 1500
  ${EndIf}
!macroend

Section "$(SecApp)" SecMain
  SectionIn RO
  !insertmacro StopRunning
  SetOutPath "$INSTDIR"
  File "${STAGE}\oxidezap.exe"
  File "${STAGE}\oxidezapd.exe"
  File "${STAGE}\oxidezap-cli.exe"
  File "${STAGE}\NotoColorEmoji-OFL.txt"
  File "/oname=oxidezap.ico" "${ICON}"

  CreateDirectory "$SMPROGRAMS\oxidezap"
  CreateShortcut "$SMPROGRAMS\oxidezap\oxidezap.lnk" "$INSTDIR\oxidezap.exe" "" "$INSTDIR\oxidezap.ico"
  CreateShortcut "$SMPROGRAMS\oxidezap\Desinstalar oxidezap.lnk" "$INSTDIR\uninstall.exe"

  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "Software\oxidezap-stickers" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayName" "${APPNAME}"
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTKEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayIcon" "$INSTDIR\oxidezap.ico"
  WriteRegStr HKCU "${UNINSTKEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTKEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegDWORD HKCU "${UNINSTKEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTKEY}" "NoRepair" 1
SectionEnd

Section "$(SecDesktop)" SecDesktop
  CreateShortcut "$DESKTOP\oxidezap.lnk" "$INSTDIR\oxidezap.exe" "" "$INSTDIR\oxidezap.ico"
SectionEnd

Function .onInit
  !insertmacro MUI_LANGDLL_DISPLAY
FunctionEnd

Section "Uninstall"
  !insertmacro StopRunning
  Delete "$INSTDIR\oxidezap.exe"
  Delete "$INSTDIR\oxidezapd.exe"
  Delete "$INSTDIR\oxidezap-cli.exe"
  Delete "$INSTDIR\NotoColorEmoji-OFL.txt"
  Delete "$INSTDIR\oxidezap.ico"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\oxidezap\oxidezap.lnk"
  Delete "$SMPROGRAMS\oxidezap\Desinstalar oxidezap.lnk"
  RMDir "$SMPROGRAMS\oxidezap"
  Delete "$DESKTOP\oxidezap.lnk"
  DeleteRegKey HKCU "${UNINSTKEY}"
  DeleteRegKey HKCU "Software\oxidezap-stickers"
SectionEnd
