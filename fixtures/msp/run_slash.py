from tui import Tui
t = Tui(["--provider","echo","--trust-workspace"])
t.pump(9); t.show("home")
t.key(b"/"); t.pump(2.5); t.show("slash-1")
for i in range(2, 9):
    t.key(b"\x1b[B"*6); t.pump(1.2); t.show(f"slash-{i}")
t.close()
