import os, pty, select, time, re, fcntl, termios, struct, sys, pyte
COLS, ROWS = 150, 50
class Tui:
    def __init__(self, args):
        self.screen = pyte.Screen(COLS, ROWS); self.stream = pyte.ByteStream(self.screen)
        ws = os.path.abspath("ws")
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.chdir(ws); os.environ["TERM"]="xterm-256color"
            os.execvp("muse", ["muse"]+args)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    def respond(self, chunk):
        out=b"\x1b[1;1R"*len(re.findall(rb'\x1b\[6n', chunk))
        for m in re.findall(rb'\x1b\](1[01]);\?(\x07|\x1b\\)', chunk):
            out += b"\x1b]"+m[0]+b";rgb:0000/0000/0000\x07"
        for m in re.findall(rb'\x1b\]4;(\d+);\?(\x07|\x1b\\)', chunk):
            out += b"\x1b]4;"+m[0]+b";rgb:0000/0000/0000\x07"
        if out: os.write(self.fd, out)
    def pump(self, sec):
        t0=time.time()
        while time.time()-t0<sec:
            r,_,_=select.select([self.fd],[],[],0.2)
            if r:
                try: d=os.read(self.fd,65536)
                except OSError: return False
                if not d: return False
                self.stream.feed(d); self.respond(d)
        return True
    def key(self, k): os.write(self.fd, k)
    def show(self, tag):
        print("#### "+tag)
        for l in self.screen.display:
            if l.strip(): print(l.rstrip())
        print()
    def close(self):
        try: self.key(b"\x03"); self.pump(0.6); self.key(b"\x03"); self.pump(0.6); os.close(self.fd)
        except Exception: pass
