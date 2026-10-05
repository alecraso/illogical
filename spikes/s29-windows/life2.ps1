# Session two: a new "daemon" connects, sees what was printed while nobody
# was attached, and finds the shell's state.
$x = "C:\s29\conpty-host\target\release\conpty-host.exe"
& $x bench expect --pipe $args[0] --send '''v'' + $x\r' --want "tick3" --also "TICKSDONE"
& $x bench expect --pipe $args[0] --send '\r' --want "v42"
