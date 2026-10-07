; Hooks into Tauri's NSIS installer (tauri.windows.conf.json).

; #505: the app was "illogical" (wtf.widgets.illogical, in
; %LOCALAPPDATA%\illogical). To NSIS, Arugula is another product, so it
; installs beside it, and an update from illogical arrives as a fresh
; install. Once Arugula is in, take the old app out with its own
; uninstaller, silently: its files, its Start menu and desktop shortcuts,
; its Add/Remove Programs entry and its autostart. It removes its
; illogical:// key only while that still points at itself, so Arugula's
; stays; it leaves its app data and the folder when the daemon's state is
; in it (RMDir without /r).
!macro NSIS_HOOK_POSTINSTALL
  ReadRegStr $R0 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\illogical" "UninstallString"
  ${If} $R0 != ""
    ReadRegStr $R1 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\illogical" "InstallLocation"
    ; Written quoted.
    StrCpy $R2 $R1 1
    ${If} $R2 == '"'
      StrCpy $R1 $R1 -1 1
    ${EndIf}
    StrCpy $R3 0
    ${If} ${FileExists} "$DESKTOP\illogical.lnk"
      StrCpy $R3 1
    ${EndIf}
    ; The old app open keeps its uninstaller from doing anything (silent,
    ; it gives up), so close it first; the daemon is its own process and
    ; its panes carry on.
    nsExec::Exec 'taskkill /IM illogical-desktop.exe /F'
    Pop $R4
    Sleep 1000
    ; _?= runs it in place, so this waits for it.
    ExecWait '$R0 /S _?=$R1' $R4
    ; Only once it's gone: its uninstaller is all that can take it out
    ; (#533: deleting it after a failed run stranded the old app).
    ReadRegStr $R5 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\illogical" "UninstallString"
    ${If} $R4 == 0
    ${AndIf} $R5 == ""
      Delete "$R1\uninstall.exe"
      RMDir "$R1"
    ${EndIf}
    ; Tauri's installer makes no shortcuts while updating, and the old
    ; app's went with it: make Arugula's.
    ${IfNot} ${FileExists} "$SMPROGRAMS\${PRODUCTNAME}.lnk"
      CreateShortcut "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
      !insertmacro SetLnkAppUserModelId "$SMPROGRAMS\${PRODUCTNAME}.lnk"
    ${EndIf}
    ${If} $R3 = 1
    ${AndIfNot} ${FileExists} "$DESKTOP\${PRODUCTNAME}.lnk"
      CreateShortcut "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
      !insertmacro SetLnkAppUserModelId "$DESKTOP\${PRODUCTNAME}.lnk"
    ${EndIf}
  ${EndIf}
!macroend
