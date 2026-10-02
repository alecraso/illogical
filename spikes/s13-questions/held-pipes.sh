#!/bin/bash
# Q2: claude-agent-acp on FIFOs held open by a keeper (stand-in for the systemd fd store), as in S7 Q4c.
# Client #1 declares elicitation, gets an AskUserQuestion elicitation and SIGKILLs itself without answering.
# 20s later client #2 attaches to the same FIFOs and answers the old elicitation by its JSON-RPC id.
set -u
cd "$(dirname "$0")"
D=work/held; rm -rf $D; mkdir -p $D
mkfifo $D/in $D/out
setsid bash -c "exec 3<>$D/in 4<>$D/out; echo \$\$ > $D/keeper.pid; exec sleep 3600" &
sleep 0.5
setsid bash -c "echo \$\$ > $D/adapter.pid; cd work/scratch; exec ../../claude-acp.sh ${V:-85} < ../held/in > ../held/out 2> ../held/adapter.err" &
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
