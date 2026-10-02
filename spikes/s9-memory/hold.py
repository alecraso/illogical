#!/usr/bin/env python3
"""Start a daemon with N idle panes and leave it running; prints its pid.
    uv run --with websockets python3 hold.py N [cols rows]"""
import sys, time, asyncio, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import bench
n = int(sys.argv[1])
d = bench.Daemon(os.path.join(bench.WORK, "illogicald"), 7741, "hold")
time.sleep(2)
bench.grow(d, n)
print(d.pid, flush=True)
if len(sys.argv) > 3:
    async def go():
        ws = await bench.resize_all(7741, int(sys.argv[2]), int(sys.argv[3]))
        while True:
            await bench.drain(ws, 5)
    asyncio.run(go())
while True:
    time.sleep(60)
