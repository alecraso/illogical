# Which hosts run, in which session, and does a client reach their pane?
$x = "C:\s29\conpty-host\target\release\conpty-host.exe"
foreach ($n in "s29-logon", "s29-s4u") {
  $h = Get-CimInstance Win32_Process | Where-Object { $_.Name -eq 'conpty-host.exe' -and $_.CommandLine -like "*--pipe $n *" }
  if ($h) {
    $s = (Get-Process -Id $h.ProcessId).SessionId
    $r = & $x bench expect --pipe $n --send "'alive' + 'x' + `$PID\r" --want "alivex" 2>&1
    "$n`: host $($h.ProcessId) in session $s; $r"
  } else { "$n`: not running" }
}
"console sessions: " + ((query session 2>$null) -join " | ")
