# Registers (or removes) the scheduled task that starts traffic-etw.exe with
# highest privileges. Must itself run elevated — once. Afterwards the tray app
# starts the collector with `schtasks /Run` and no UAC prompt.
#
#   powershell -ExecutionPolicy Bypass -File etw-task.ps1 -ExePath <traffic-etw.exe>
#   powershell -ExecutionPolicy Bypass -File etw-task.ps1 -Unregister
param(
    [string]$ExePath,
    [switch]$Unregister
)
$ErrorActionPreference = 'Stop'

$FolderName = 'AppTraffic'
$TaskName = 'ETW Collector'

$svc = New-Object -ComObject Schedule.Service
$svc.Connect()
$root = $svc.GetFolder('\')
try { $folder = $root.GetFolder($FolderName) } catch { $folder = $null }

if ($Unregister) {
    if ($folder) {
        try { $folder.DeleteTask($TaskName, 0) } catch {}
        try { $root.DeleteFolder($FolderName, 0) } catch {}
    }
    exit 0
}

if (-not $ExePath -or -not (Test-Path $ExePath)) { throw "traffic-etw.exe not found: $ExePath" }
if (-not $folder) { $folder = $root.CreateFolder($FolderName) }

$def = $svc.NewTask(0)
$def.RegistrationInfo.Description = 'AppTraffic: per-app network usage collector (ETW). Started on demand by the AppTraffic tray app.'
$def.Principal.UserId = [Security.Principal.WindowsIdentity]::GetCurrent().Name
$def.Principal.LogonType = 3   # TASK_LOGON_INTERACTIVE_TOKEN: only while the user is logged on
$def.Principal.RunLevel = 1    # TASK_RUNLEVEL_HIGHEST
$s = $def.Settings
$s.DisallowStartIfOnBatteries = $false
$s.StopIfGoingOnBatteries = $false
$s.ExecutionTimeLimit = 'PT0S' # no limit; the collector exits by itself when the app disconnects
$s.MultipleInstances = 2       # TASK_INSTANCES_IGNORE_NEW
$s.AllowDemandStart = $true
$s.Hidden = $true
$action = $def.Actions.Create(0)
$action.Path = $ExePath

# Administrators/SYSTEM full control; the current user may read and run it
# without elevation (that's what lets the tray app start it silently).
$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$sddl = "D:(A;;FA;;;BA)(A;;FA;;;SY)(A;;GRGX;;;$sid)"
# 6 = TASK_CREATE_OR_UPDATE, 3 = TASK_LOGON_INTERACTIVE_TOKEN
$null = $folder.RegisterTaskDefinition($TaskName, $def, 6, $null, $null, 3, $sddl)
