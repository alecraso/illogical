# In the S4U pane: can the user's DPAPI (what Credential Manager and Git
# Credential Manager use) protect and unprotect? And network identity?
$x = "C:\s29\conpty-host\target\release\conpty-host.exe"
$cmd = 'try { $b = [Security.Cryptography.ProtectedData]::Protect([byte[]](1,2,3), $null, ''CurrentUser''); $c = [Security.Cryptography.ProtectedData]::Unprotect($b, $null, ''CurrentUser''); ''dpapi'' + ''-ok'' } catch { ''dpapi'' + ''-fail '' + $_.Exception.Message }\r'
& $x bench expect --pipe $args[0] --send $cmd --want "dpapi-ok"
& $x bench expect --pipe $args[0] --send '(cmdkey /list | Select-Object -First 3) -join '' / ''\r' --want "Currently"
