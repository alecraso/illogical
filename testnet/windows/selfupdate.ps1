# The daemon updates itself on Windows (#391): live checks in the Windows VM.
#
# Needs (prep.ps1 makes them): $Su\bin\<OLD|NEW>\{illogicald,illogical,conpty,OpenConsole}
# and the fake release in $Su\rel (NEW's zip + SHA256SUMS, BAD's zip with a
# wrong sum), served by serve.ps1 on 127.0.0.1:$Port. The user must be logged
# in to the desktop: the logon task runs in that session.
#
#   powershell -ExecutionPolicy Bypass -File selfupdate.ps1
#
# Claims: check (OLD finds NEW, apply true), apply (POST /api/update/apply ->
# NEW within 2 min), panes (a pane from before keeps its process and keeps
# counting), bad (`illogicald update -y` refuses BAD's checksum; still NEW).
# Leaves NEW installed as the logon task, with --update-url in daemon-args.json.
param([string]$Su = "C:\s29\su", [int]$Port = 8791,
      [string]$Old = "0.23.98", [string]$New = "0.23.99", [string]$Bad = "0.23.100")
$ErrorActionPreference = "Stop"
$state = "$env:LOCALAPPDATA\illogical\state"
$prog = "$env:LOCALAPPDATA\Programs\illogical"
$url = "http://127.0.0.1:$Port/releases/latest"
$log = "$state\illogicald.log"
$fail = 0
function Say($claim, $ok, $why) {
  $script:fail += [int](-not $ok)
  "{0,-6} {1}: {2}" -f $claim, $(if ($ok) { "PASS" } else { "FAIL" }), $why
}
function Api($method, $path) {
  $listen = (Get-Content "$state\listen" -Raw).Trim()
  $tok = (Get-Content "$state\local-token" -Raw).Trim()
  Invoke-RestMethod -Method $method -Uri "http://$listen$path" -Headers @{ Authorization = "Bearer $tok" } -TimeoutSec 10
}
function Until($secs, [scriptblock]$cond) {
  $end = (Get-Date).AddSeconds($secs)
  while ((Get-Date) -lt $end) {
    try { $v = & $cond; if ($v) { return $v } } catch { }
    Start-Sleep -Milliseconds 1000
  }
  $null
}

# The fake release, in the desktop session (ssh's processes end with it).
Set-Content "$Su\rel\latest.txt" $New
$up = try { (Invoke-WebRequest "http://127.0.0.1:$Port/releases/download/v$New/SHA256SUMS" -UseBasicParsing -TimeoutSec 3).StatusCode -eq 200 } catch { $false }
if (-not $up) {
  schtasks /Create /TN su-serve /SC ONCE /ST 00:00 /IT /F /TR "powershell -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File $Su\serve.ps1 -Root $Su\rel -Port $Port" | Out-Null
  schtasks /Run /TN su-serve | Out-Null
  if (-not (Until 20 { (Invoke-WebRequest "http://127.0.0.1:$Port/releases/download/v$New/SHA256SUMS" -UseBasicParsing -TimeoutSec 3).StatusCode -eq 200 })) {
    throw "the fake release isn't answering on :$Port"
  }
}
"fake release on :$Port, latest $New"

# OLD as the logon task, pointed at the fake. A cached check from another
# URL doesn't count, but one from an earlier run of this does: drop it.
Remove-Item "$state\update-check.json" -ea 0
& "$Su\bin\$Old\illogicald.exe" install '--' --update-url $url
if ($LASTEXITCODE) { throw "installing $Old failed" }
$u = Until 60 { $s = Api GET /api/update; if ($s.current -eq $Old -and $s.latest) { $s } }
"GET /api/update: " + ($u | ConvertTo-Json -Compress)
Say check ($u -and $u.current -eq $Old -and $u.latest -eq $New -and $u.newer -and $u.apply) "current $($u.current), latest $($u.latest), newer $($u.newer), apply $($u.apply), kind $($u.kind)"

# A pane that counts, started before the update.
Remove-Item "$Su\count.txt" -ea 0
Set-Content "$Su\count.ps1" '$i = 0; while ($true) { $i++; Set-Content C:\s29\su\count.txt "$PID $i"; Start-Sleep 1 }'
$pane = (& "$prog\illogical.exe" run --session work powershell -NoProfile -ExecutionPolicy Bypass -File "$Su\count.ps1" | Out-String).Trim()
$c0 = Until 20 { Get-Content "$Su\count.txt" -ea 0 }
$panePid, $n0 = "$c0".Split(" ")
"pane ${pane}: counter pid $panePid at $n0"

# apply
$before = Get-Process -Name illogicald | Where-Object { $_.Path -like "$prog\*" -or $_.Path -like "$prog\old\*" } | Select-Object -First 1
$logAt = (Get-Item $log).Length
$t0 = Get-Date
"POST /api/update/apply: " + ((Api POST /api/update/apply) | ConvertTo-Json -Compress)
$stages = @()
$a = Until 150 {
  $s = Api GET /api/update
  if ($s.applying -and $stages[-1] -ne $s.applying.stage) { $script:stages += $s.applying.stage }
  if ($s.applying.stage -eq "failed") { return $s }
  if ($s.current -eq $New) { $s }
}
$took = [int]((Get-Date) - $t0).TotalSeconds
$exeV = (& "$prog\illogicald.exe" --version)
$fs = [IO.File]::Open($log, "Open", "Read", "ReadWrite")
$fs.Seek($logAt, "Begin") | Out-Null
$newLog = [IO.StreamReader]::new($fs).ReadToEnd(); $fs.Close()
$after = Get-Process -Name illogicald | Where-Object { $_.Path -like "$prog\illogicald.exe" } | Select-Object -First 1
"stages seen: $($stages -join ' -> '); daemon pid $($before.Id) -> $($after.Id)"
# The old daemon should stop when the install asks; ending it after 10 s
# means something held it (the spawn_blocking bug fixed in da5a89e).
$upd = (Get-Content "$state\update.log" -ea 0 | Out-String)
$killed = $upd -match "didn't stop when asked"
Say apply ($a -and $a.current -eq $New -and $exeV -eq "illogicald $New" -and -not $killed) "after ${took}s the daemon says $($a.current) (applying $($a.applying | ConvertTo-Json -Compress)); $prog\illogicald.exe says '$exeV'; old daemon had to be ended: $killed"
$fellBack = $newLog -match "can't leave the task's job"
"breakaway: " + $(if ($fellBack) { "FELL BACK (the install ran inside the task's job)" } else { "CREATE_BREAKAWAY_FROM_JOB worked (no fallback warning in the log)" })
"--- daemon log since apply (update lines) ---"
$newLog -split "`n" | Where-Object { $_ -match "updat|install|job|stop|start|listening" } | Select-Object -Last 25
"--- $state\update.log ---"
Get-Content "$state\update.log" -ea 0

# panes
$c1 = Get-Content "$Su\count.txt"; Start-Sleep 3; $c2 = Get-Content "$Su\count.txt"
$p1, $n1 = "$c1".Split(" "); $p2, $n2 = "$c2".Split(" ")
$alive = [bool](Get-Process -Id $panePid -ea 0)
$listed = (& "$prog\illogical.exe" ls | Out-String) -match [regex]::Escape(($pane -split "\s+")[0])
Say panes ($alive -and $p2 -eq $panePid -and [int]$n2 -gt [int]$n1 -and [int]$n1 -gt [int]$n0 -and $listed) "pid $panePid alive $alive; count $n0 before -> $n1 -> $n2 (3 s later, pid $p2); listed by the new daemon $listed"

# bad
Set-Content "$Su\rel\latest.txt" $Bad
$env:ILLOGICAL_UPDATE_URL = $url
$ErrorActionPreference = "Continue"   # its stderr isn't a PowerShell error
$out = (& "$prog\illogicald.exe" update -y 2>&1 | ForEach-Object { "$_" } | Out-String).Trim()
$code = $LASTEXITCODE
$ErrorActionPreference = "Stop"
Remove-Item Env:ILLOGICAL_UPDATE_URL
Set-Content "$Su\rel\latest.txt" $New
$s = Api GET /api/update
$exeV = (& "$prog\illogicald.exe" --version)
"illogicald update -y (exit $code):"; $out
Say bad ($code -ne 0 -and $out -match "doesn't match" -and $s.current -eq $New -and $exeV -eq "illogicald $New") "exit $code, says doesn't match: $($out -match "doesn't match"); daemon $($s.current), exe '$exeV'"

& "$prog\illogical.exe" close ($pane -split "\s+")[0] | Out-Null
"$fail failed"
exit $fail
