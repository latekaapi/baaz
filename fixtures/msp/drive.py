"""Drive the real `muse` TUI over a pty and snapshot its screen.

Forks `muse` (default args: `--provider echo --trust-workspace`) under a pty,
answers the terminal's own query escapes (cursor position, OSC color
queries) so the TUI does not stall, then plays a fixed key script: open the
`/` menu and arrow through it. As written it never types a prompt or presses
Enter, so it never reaches `turn/start` and sends no turn — `--provider echo`
here only picks a route (D19: a signed-in login still bills it), and this
script never exercises that route. If you extend the key script to submit
text, that submission is a billed turn under the spend rule in `CLAUDE.md`;
count it from Muse's own `session.jsonl`, not from this script.
"""

import os, pty, select, time, re, fcntl, termios, struct, sys
ws = os.path.abspath("ws")
args = sys.argv[1:] or ["--provider","echo","--trust-workspace"]
pid, fd = pty.fork()
if pid == 0:
    os.chdir(ws); os.environ["TERM"]="xterm-256color"
    os.execvp("muse", ["muse"]+args)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 50, 150, 0, 0))
buf=b""
def respond(chunk):
    out=b""
    out += b"\x1b[1;1R"*len(re.findall(rb'\x1b\[6n', chunk))
    for m in re.findall(rb'\x1b\](1[01]);\?(\x07|\x1b\\)', chunk):
        out += b"\x1b]"+m[0]+b";rgb:0000/0000/0000\x07"
    for m in re.findall(rb'\x1b\]4;(\d+);\?(\x07|\x1b\\)', chunk):
        out += b"\x1b]4;"+m[0]+b";rgb:0000/0000/0000\x07"
    if out: os.write(fd,out)
def pump(sec):
    global buf
    t0=time.time()
    while time.time()-t0<sec:
        r,_,_=select.select([fd],[],[],0.2)
        if r:
            try: d=os.read(fd,65536)
            except OSError: return False
            if not d: return False
            buf+=d; respond(d)
    return True
def clean(b):
    t=re.sub(rb'\x1b\][^\x07\x1b]*(\x07|\x1b\\)',b'',b)
    t=re.sub(rb'\x1b[\[\(][0-9;?]*[a-zA-Z]',b'',t)
    t=re.sub(rb'\x1b[=>]',b'',t)
    return t.decode('utf8','replace')
pump(10)
def snap(tag, keys=b"", wait=2.5):
    global buf
    if keys: os.write(fd, keys)
    buf=b""; pump(wait)
    open(f"tui-{tag}.txt","w").write(clean(buf))
    print("#### "+tag); print(clean(buf)[-3500:]); print()
snap("home")
snap("slash", b"/", 3)
snap("slash-down", b"\x1b[B"*8, 2)
