# A host nobody connects to closes its pane after --grace.
$x = "C:\s29\conpty-host\target\release\conpty-host.exe"
$out = & $x spawn --pipe s29-grace2 --grace 5 -- pwsh -NoLogo -NoProfile
$hostPid = [int]([regex]::Match($out, 'host pid (\d+)').Groups[1].Value)
Start-Sleep 1
$pane = (Get-CimInstance Win32_Process | Where-Object { $_.ParentProcessId -eq $hostPid -and $_.Name -eq 'pwsh.exe' }).ProcessId
"host $hostPid, pane $pane, both alive at 1 s: " + [bool]((Get-Process -Id $hostPid -EA SilentlyContinue) -and (Get-Process -Id $pane -EA SilentlyContinue))
Start-Sleep 11
"at 12 s: host alive " + [bool](Get-Process -Id $hostPid -EA SilentlyContinue) + ", pane alive " + [bool](Get-Process -Id $pane -EA SilentlyContinue)
