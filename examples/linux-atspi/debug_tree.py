#!/usr/bin/env python3
"""Debug: dump the live AT-SPI tree of the fixture, drive a few taps, re-dump.
Used to diagnose GtkStack visibility in the accessibility tree."""
import sys, time
import gi
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi

Atspi.init()

def find_app(target):
    d = Atspi.get_desktop(0)
    for i in range(d.get_child_count()):
        a = d.get_child_at_index(i)
        try:
            if a and target.lower() in (a.get_name() or "").lower():
                return a
        except Exception:
            pass
    return None

def dump(node, depth=0):
    try:
        rn = node.get_role_name()
        nm = node.get_name() or ""
        try:
            aid = node.get_accessible_id() or ""
        except Exception:
            aid = "<no-aid-api>"
        try:
            ni = node.get_action_iface()
            nacts = ni.get_n_actions() if ni else 0
        except Exception:
            nacts = "?"
        print("  " * depth + f"[{rn}] name={nm!r} aid={aid!r} acts={nacts}")
        for i in range(node.get_child_count()):
            c = node.get_child_at_index(i)
            if c is not None:
                dump(c, depth + 1)
    except Exception as e:
        print("  " * depth + f"<err {e}>")

def press(app, label):
    found = [None]
    def visit(n):
        if found[0]: return
        try:
            if (n.get_name() or "") == label:
                found[0] = n; return
            for i in range(n.get_child_count()):
                visit(n.get_child_at_index(i))
        except Exception:
            pass
    visit(app)
    if found[0]:
        ai = found[0].get_action_iface()
        if ai and ai.get_n_actions() > 0:
            ai.do_action(0)
            print(f"pressed {label!r}")
            return True
    print(f"could NOT press {label!r}")
    return False

app = find_app(sys.argv[1] if len(sys.argv) > 1 else "Fixture")
if not app:
    print("APP NOT FOUND"); sys.exit(1)
print("=== INITIAL TREE ===")
dump(app)
print("\n=== press Open Help ===")
press(app, "Open Help")
time.sleep(1.0)
print("=== TREE AFTER Open Help ===")
dump(app)
print("\n=== press Get Stuck ===")
press(app, "Get Stuck")
time.sleep(1.0)
print("=== TREE AFTER Get Stuck (dead-end) ===")
dump(app)
