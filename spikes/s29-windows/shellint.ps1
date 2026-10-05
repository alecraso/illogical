# Shell integration without a profile file: pwsh -Command runs inline code,
# which the execution policy doesn't cover. Wraps whatever prompt the
# user's profile set up.
$init = @'
$global:__ill_prompt = $function:prompt
function global:prompt {
  $e = [char]27; $code = if ($?) { 0 } else { 1 }
  $p = $executionContext.SessionState.Path.CurrentLocation
  $uri = 'file://' + $env:COMPUTERNAME + '/' + ($p.ProviderPath -replace '\\','/')
  "$e]133;D;$code$e\$e]7;$uri$e\$e]133;A$e\" + (& $global:__ill_prompt) + "$e]133;B$e\"
}
'@
$enc = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($init))
$x = "C:\s29\conpty-host\target\release\conpty-host.exe"
foreach ($dll in "", "C:\s29\conpty\conpty.dll") {
  if ($dll) { $env:S29_CONPTY_DLL = $dll; "-- conpty.dll" } else { Remove-Item Env:S29_CONPTY_DLL -ErrorAction SilentlyContinue; "-- inbox" }
  $pipe = "s29-si-" + $dll.Length
  & $x spawn --pipe $pipe --grace 20 -- pwsh -NoLogo -NoExit -EncodedCommand $enc | Out-Null
  Start-Sleep 2
  & $x bench expect --pipe $pipe --send 'cd C:\Windows\r' --want '\e]7;file://' --also '\e]133;A'
  & $x bench expect --pipe $pipe --send 'cd C:\Windows\r' --want 'C:/Windows' --also '\e]133;B'
}
