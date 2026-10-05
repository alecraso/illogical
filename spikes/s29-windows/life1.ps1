# Session one: start a pane, type into it, and leave while it prints.
# (No double quotes in what's typed: Windows PowerShell splits native args on them.)
$env:S29_CONPTY_DLL = "C:\s29\conpty\conpty.dll"
$x = "C:\s29\conpty-host\target\release\conpty-host.exe"
& $x spawn --pipe $args[0] --grace 60 -- pwsh -NoLogo -NoProfile
Start-Sleep 2
& $x bench put --pipe $args[0] --text '$x = 42; 1..8 | % { ''tick'' + $_; Start-Sleep 1 }; ''TICKS'' + ''DONE''\r'
