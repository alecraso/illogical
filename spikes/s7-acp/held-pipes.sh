#!/bin/bash
# Q4c: claude-agent-acp on FIFOs held open by a keeper (stand-in for the systemd fd store).
# Client #1 starts a turn and SIGKILLs itself with a permission request pending.
# 20s later client #2 reattaches to the same FIFOs, answers the old request and sends turn 3.
set -u
cd "$(dirname "$0")"
D=work/held; rm -rf $D; mkdir -p $D
mkfifo $D/in $D/out
# keeper: holds both FIFOs open read-write so neither side ever sees EOF
setsid bash -c "exec 3<>$D/in 4<>$D/out; echo \$\$ > $D/keeper.pid; exec sleep 3600" &
sleep 0.5
# the adapter, in its own session (like a scope), stdio on the FIFOs
setsid bash -c "echo \$\$ > $D/adapter.pid; exec ./claude-acp.sh < $D/in > $D/out 2> $D/adapter.err" &
sleep 0.5
echo "keeper $(cat $D/keeper.pid) adapter $(cat $D/adapter.pid)"
STEP=1 mise exec bun@1.4.2 -- bun held-client.ts
echo "client #1 gone; adapter alive? $(ps -p $(cat $D/adapter.pid) -o stat= || echo no)"
sleep 20
echo "after 20s adapter alive? $(ps -p $(cat $D/adapter.pid) -o stat= || echo no)"
STEP=2 mise exec bun@1.4.2 -- bun held-client.ts
echo "client #2 done"
kill "$(cat $D/adapter.pid)" "$(cat $D/keeper.pid)" 2>/dev/null
sleep 1
pgrep -f "$D" || echo "no leftovers"
