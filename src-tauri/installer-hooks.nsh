; AppTraffic 的 NSIS 钩子（Tauri 安装包）。
;
; 提权的 ETW 采集程序在主程序退出后还会活最多 30 秒，会占着 exe 不放；替换文件前先结束它。
!macro NSIS_HOOK_PREINSTALL
  nsExec::Exec 'taskkill /F /IM traffic-etw.exe'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec 'taskkill /F /IM traffic-etw.exe'
  ; 删掉以最高权限启动采集程序的计划任务；升级时（旧版本的卸载程序也会跑到这里）保留，
  ; 这样不用再弹一次 UAC
  ${If} $UpdateMode <> 1
    nsExec::Exec 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\etw-task.ps1" -Unregister'
  ${EndIf}
!macroend
