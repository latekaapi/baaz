#!/usr/bin/env python3
"""Drive the muse TUI in a pty, type '/', and dump what the palette shows."""
import os, pty, select, sys, time, re

ws = os.path.join(os.path.dirname(os.path.abspath(__file__)), "ws")
cmd = ["muse", "--provider", "echo", "--workspace", ws]
pid, fd = pty.fork()
if pid == 0:
    os.chdir(ws)
    os.environ["TERM"] = "xterm-256color"
    os.environ["LINES"] = "60"; os.environ["COLUMNS"] = "160"
    os.execvp(cmd[0], cmd)

import fcntl, termios, struct
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 60, 160, 0, 0))

buf = b""
def pump(sec):
    global buf
    t0 = time.time()
    while time.time() - t0 < sec:
        r, _, _ = select.select([fd], [], [], 0.2)
        if r:
            try: d = os.read(fd, 65536)
            except OSError: break
            if not d: break
            buf += d

pump(6)
os.write(fd, b"/")
pump(3)
open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "tui-slash-raw.txt"), "wb").write(buf)
# also page down the palette
for _ in range(6):
    os.write(fd, b"\x1b[B")
    pump(0.4)
open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "tui-slash-raw2.txt"), "wb").write(buf)
os.write(fd, b"\x03")
pump(1)
os.write(fd, b"\x03")
pump(1)
try: os.close(fd)
except Exception: pass
text = re.sub(rb"\x1b\[[0-9;?]*[a-zA-Z]", b"", buf).decode("utf8", "replace")
text = re.sub(r"\x1b\][^\x07]*\x07", "", text)
lines = [l.rstrip() for l in text.split("\n")]
seen = set()
for l in lines:
    s = l.strip()
    if "/" in s and s not in seen:
        seen.add(s)
        print(s[:160])
