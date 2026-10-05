# Another local user tries the daemon's pipe and a pane host's pipe.
$x = "C:\s29\conpty-host\target\release\conpty-host.exe"
$pw = ConvertTo-SecureString "S29-other-pass!" -AsPlainText -Force
if (-not (Get-LocalUser s29other -EA SilentlyContinue)) { New-LocalUser s29other -Password $pw | Out-Null }
$cred = New-Object System.Management.Automation.PSCredential("s29other", $pw)
$s = Start-Process $x -ArgumentList "http-serve","--pipe","s29-http-$PID","--tcp","127.0.0.1:7998" -PassThru -WindowStyle Hidden
& $x spawn --pipe s29-pane-$PID --grace 30 -- pwsh -NoLogo -NoProfile | Out-Null
Start-Sleep 2
New-Item -ItemType Directory -Force C:\s29\other | Out-Null
icacls C:\s29\other /grant "s29other:(OI)(CI)F" | Out-Null
icacls C:\s29\conpty-host /grant "s29other:(OI)(CI)RX" /T | Out-Null
$probe = 'foreach ($p in ''s29-http-$PID'',''s29-pane-$PID'') { try { $f = [IO.File]::Open(''\\.\pipe\'' + $p, ''Open'', ''ReadWrite''); $p + '': opened''; $f.Close() } catch { $p + '': '' + $_.Exception.Message } }'
# As that user, through a one-off scheduled task (Start-Process -Credential
# from an ssh session fails with STATUS_DLL_INIT_FAILED: no desktop).
Set-Content C:\s29\other\probe.ps1 ($probe + ' *> C:\s29\other\out.txt')
$a = New-ScheduledTaskAction -Execute powershell -Argument "-NoProfile -ExecutionPolicy Bypass -File C:\s29\other\probe.ps1"
Register-ScheduledTask -TaskName s29-other -Action $a -User s29other -Password "S29-other-pass!" -Force | Out-Null
Remove-Item C:\s29\other\out.txt -EA SilentlyContinue
Start-ScheduledTask s29-other
for ($i = 0; $i -lt 30 -and -not (Test-Path C:\s29\other\out.txt); $i++) { Start-Sleep 1 }
Start-Sleep 2
Get-Content C:\s29\other\out.txt
"task result: " + (Get-ScheduledTaskInfo s29-other).LastTaskResult
Unregister-ScheduledTask s29-other -Confirm:$false
"and as the owner: " + (& $x bench expect --pipe s29-pane-$PID --send 'whoami\r' --want '\')
Stop-Process $s
