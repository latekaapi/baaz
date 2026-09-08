from tui import Tui
t = Tui(["--provider","echo","--trust-workspace"])
t.pump(9)
t.key(b"/keymap"); t.pump(1.5); t.key(b"\t"); t.pump(0.8); t.key(b"\r"); t.pump(2.5); t.show("keymap")
t.key(b"\x1b"); t.pump(1)
t.key(b"/effort"); t.pump(1.2); t.key(b"\t"); t.pump(0.8); t.key(b"\r"); t.pump(2.5); t.show("effort")
t.key(b"\x1b"); t.pump(1)
t.key(b"/status"); t.pump(1.2); t.key(b"\t"); t.pump(0.8); t.key(b"\r"); t.pump(3); t.show("status")
t.close()
