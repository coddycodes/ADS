; Passive NSIS is not silent. The stock template waits on a file-in-use dialog
; when the installed executable is still mapped, and CI never dismisses it.
; The updater has already exited the host; this hook clears a brief lock before
; CheckIfAppIsRunning and File run. A lock that outlives the bounded retry still
; reaches that template dialog.
!macro NSIS_HOOK_PREINSTALL
  StrCpy $R9 0
  bibcode_preinstall_retry:
    nsis_tauri_utils::KillProcessCurrentUser "${MAINBINARYNAME}.exe"
    Pop $R8
    ClearErrors
    Delete "$INSTDIR\${MAINBINARYNAME}.exe"
    ${If} ${FileExists} "$INSTDIR\${MAINBINARYNAME}.exe"
      IntOp $R9 $R9 + 1
      ${If} $R9 < 60
        Sleep 250
        Goto bibcode_preinstall_retry
      ${EndIf}
    ${EndIf}
!macroend
