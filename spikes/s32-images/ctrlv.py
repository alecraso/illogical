# S32: run claude in a pty, press Ctrl+V, and print any clipboard query it
# sends (OSC 52, 5522, 1337). `python3 ctrlv.py ssh|local DIR` (DIR trusted).
# Run claude in a pty, press Ctrl+V, and record what it writes to the terminal.
import os, pty, sys, time, select, re, json
mode = sys.argv[1]  # "local" or "ssh"
env = dict(os.environ)
env.pop("ILLOGICAL_PANE", None)
if mode == "ssh":
    env.update(SSH_CONNECTION="10.0.0.2 50000 10.0.0.1 22", SSH_CLIENT="10.0.0.2 50000 22", SSH_TTY="/dev/ttys999")
pid, fd = pty.fork()
if pid == 0:
    os.chdir(sys.argv[2]); os.execvpe("claude", ["claude"], env)
import fcntl, termios, struct
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 100, 0, 0))
def drain(t):
    out=b""; end=time.time()+t
    while time.time()<end:
        r,_,_=select.select([fd],[],[],0.1)
        if r:
            try: d=os.read(fd,65536)
            except OSError: break
            out+=d
            # answer terminal queries the way a basic terminal would
            if b"\x1b[c" in d or b"\x1b[0c" in d: os.write(fd,b"\x1b[?62;22c")
    return out
boot=drain(8)
os.write(fd,b"\x16"); after=drain(4)
os.write(fd,b"\x03"); drain(0.5); os.write(fd,b"\x03"); drain(1)
def oscs(b): return sorted(set(m.decode('latin1') for m in re.findall(rb"\x1b\](?:52|5522|1337)[^\x07\x1b]{0,80}", b)))
print(json.dumps({"mode":mode,"boot_queries":oscs(boot),"after_ctrl_v":oscs(after),
  "after_text":re.sub(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b\][^\x07]*\x07","",after.decode('utf8','replace'))[-400:]},indent=1))
open(f"{os.path.dirname(__file__)}/ctrlv-{mode}.bin","wb").write(after)
