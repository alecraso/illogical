# Two ways to start a "daemon" (here a pane host running pwsh) with no one
# at the keyboard:
#   s29-logon: a logon task, as the user, interactive (illogicald install's plan)
#   s29-s4u:   at startup, as the user, S4U (runs without a logon; no password stored)
$x = "C:\s29\conpty-host\target\release\conpty-host.exe"
$set = New-ScheduledTaskSettingsSet -ExecutionTimeLimit 0 -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew
foreach ($t in @(
    @{ Name = "s29-logon"; Trigger = (New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME); Logon = "Interactive" },
    @{ Name = "s29-s4u"; Trigger = (New-ScheduledTaskTrigger -AtStartup); Logon = "S4U" })) {
  $a = New-ScheduledTaskAction -Execute $x -Argument "host --pipe $($t.Name) --grace 86400 -- pwsh -NoLogo -NoProfile"
  $p = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType $t.Logon -RunLevel Limited
  Register-ScheduledTask -TaskName $t.Name -Action $a -Trigger $t.Trigger -Principal $p -Settings $set -Force | Out-Null
  "registered $($t.Name) ($($t.Logon))"
}
