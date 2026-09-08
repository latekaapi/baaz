from tui import Tui
t = Tui(["--provider","echo","--trust-workspace"])
t.pump(9)
t.key(b"/keymap"); t.pump(1.5); t.key(b"\t"); t.pump(0.8); t.key(b"\r"); t.pump(2.5)
for i in range(8):
    t.show(f"keymap-{i}"); t.key(b"\x1b[B"*5); t.pump(1.0)
t.close()
