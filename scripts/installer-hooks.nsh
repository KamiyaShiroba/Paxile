; Included by Tauri's stock NSIS template (utils.nsh is already available).
; Restart Manager scopes shutdown to these exact installed executable paths.
; NEVER register paxile_spy.dll: it is loaded into unrelated user applications.
; Never taskkill by image name, process tree, or enumerate target applications.
!macro PAXILE_STOP_INSTALLED_PROCESSES
  !insertmacro CheckIfAppIsRunning "$INSTDIR\${MAINBINARYNAME}.exe" "Paxile"
  !insertmacro CheckIfAppIsRunning "$INSTDIR\paxile-probe.exe" "Paxile core"
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro PAXILE_STOP_INSTALLED_PROCESSES
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro PAXILE_STOP_INSTALLED_PROCESSES
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Keep autostart enabled through an in-place update, but remove stale entries
  ; on a real uninstall. Tauri package_info uses configured productName.
  ${If} $UpdateMode != 1
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${PRODUCTNAME}"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run" "${PRODUCTNAME}"
  ${EndIf}
!macroend
