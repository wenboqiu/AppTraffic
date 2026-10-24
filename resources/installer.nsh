; electron-builder NSIS hooks for AppTraffic.

; The elevated ETW helper outlives the app by up to 30 s and would keep its exe
; locked; stop it before files are replaced.
!macro customInit
  nsExec::Exec 'taskkill /F /IM traffic-etw.exe'
!macroend

!macro customUnInstall
  nsExec::Exec 'taskkill /F /IM traffic-etw.exe'
  ; Remove the scheduled task that starts the helper elevated, but keep it across
  ; upgrades (the old version's uninstaller runs then too) so no new UAC prompt is needed
  ${ifNot} ${isUpdated}
    nsExec::Exec 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\resources\resources\etw-task.ps1" -Unregister'
  ${endIf}
!macroend
