#!/usr/bin/env python3
"""Unit tests for the widget-tree -> canonical-descriptor mapping.

The LIVE GTK/AT-SPI capture cannot run headless here (no display, no a11y bus),
so we exercise the mapping layer two ways:

  1. node_from_attrs (the PURE single-node mapper both live walks funnel
     through): role mapping, id/type extraction, value gating, live-region and
     progressbar promotion.

  2. build_node_atk / build_node_atspi against SYNTHETIC accessibles (fakes that
     mimic the ATK / AT-SPI Python interfaces), proving the tree walk folds a
     whole widget tree into the canonical Node tree and that the resulting
     descriptor matches a hand-built Node tree's descriptor.

What is NOT exercised: a real GtkWindow / Qt widget, the AT-SPI bus, signal
synthesis. Those need a Linux display + a11y stack. The signature itself is
proven against the 24 golden vectors in test_parity.py.

Run:
    python3 sdk/reproit-linux/tests/test_capture.py
"""

import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SDK_ROOT = os.path.dirname(HERE)
sys.path.insert(0, SDK_ROOT)

from reproit_linux import capture  # noqa: E402
from reproit_linux.signature import Node, descriptor  # noqa: E402


# --- synthetic ATK / AT-SPI fakes -------------------------------------------

class _Role:
    """Mimics an Atk.Role / Atspi.Role enum member: it has a value_name like
    'ATSPI_ROLE_PUSH_BUTTON' that the walk strips to 'PUSH_BUTTON'."""
    def __init__(self, name):
        self.value_name = "ATSPI_ROLE_" + name


class _Attr:
    def __init__(self, name, value):
        self.name = name
        self.value = value


class FakeAtk:
    """A synthetic ATK accessible (GTK in-process interface subset)."""
    def __init__(self, role_name, accessible_id=None, name="", attrs=None,
                 text=None, current_value=None, children=None):
        self._role = _Role(role_name)
        self._id = accessible_id
        self._name = name
        self._attrs = attrs or {}
        self._text = text
        self._current_value = current_value
        self._children = children or []

    def get_role(self):
        return self._role

    def get_name(self):
        return self._name

    def get_attributes(self):
        # ATK returns a list of AtkAttribute structs (.name / .value), and the id
        # is exposed as an 'accessible-id' attribute.
        out = [_Attr(k, v) for k, v in self._attrs.items()]
        if self._id:
            out.append(_Attr("accessible-id", self._id))
        return out

    # AtkText
    def get_character_count(self):
        return len(self._text) if self._text is not None else 0

    def get_text(self, start, end):
        return self._text

    # AtkValue
    def get_current_value(self):
        return self._current_value

    def get_n_accessible_children(self):
        return len(self._children)

    def ref_accessible_child(self, i):
        return self._children[i]


class FakeAtspi:
    """A synthetic AT-SPI accessible (Qt / bus interface subset)."""
    class _ValueIface:
        def __init__(self, v):
            self._v = v

        def get_current_value(self):
            return self._v

    class _TextIface:
        def __init__(self, t):
            self._t = t

        def get_character_count(self):
            return len(self._t)

        def get_text(self, start, end):
            return self._t

    def __init__(self, role_name, accessible_id=None, name="", attrs=None,
                 text=None, current_value=None, children=None):
        self._role = _Role(role_name)
        self._id = accessible_id
        self._name = name
        self._attrs = attrs or {}
        self._text = text
        self._current_value = current_value
        self._children = children or []

    def get_role(self):
        return self._role

    def get_name(self):
        return self._name

    def get_accessible_id(self):
        return self._id or ""

    def get_attributes(self):
        return dict(self._attrs)

    def get_value_iface(self):
        return FakeAtspi._ValueIface(self._current_value) if self._current_value is not None else None

    def get_text_iface(self):
        return FakeAtspi._TextIface(self._text) if self._text is not None else None

    def get_child_count(self):
        return len(self._children)

    def get_child_at_index(self, i):
        return self._children[i]


# --- the checks --------------------------------------------------------------

FAILS = []


def check(label, got, want):
    if got != want:
        FAILS.append("  %s\n      expected %r\n      got      %r" % (label, want, got))


def test_node_from_attrs():
    # Role mapping + id.
    n = capture.node_from_attrs("PUSH_BUTTON", "submit")
    check("button role", (n.role, n.id), ("button", "submit"))

    # Password input -> textfield with type=password.
    n = capture.node_from_attrs("PASSWORD_TEXT", "pwd", value="hunter2")
    check("password type", (n.role, n.type), ("textfield", "password"))
    check("password value (NONEMPTY-bearing)", n.value, "hunter2")

    # Spin button -> textfield type=number, value retained (value-role).
    n = capture.node_from_attrs("SPIN_BUTTON", "qty", value="3")
    check("spin number type", (n.role, n.type, n.value), ("textfield", "number", "3"))

    # Unknown role -> node.
    n = capture.node_from_attrs("CAROUSEL", None)
    check("unknown role -> node", n.role, "node")

    # Chrome role with a stray value: the raw value may ride on the Node, but it
    # MUST NOT appear in the descriptor / V: section (rule 1, enforced by the
    # oracle's value-gate, not by nulling at capture).
    n = capture.node_from_attrs("LABEL", "lbl", value="Welcome")
    check("chrome label role", n.role, "text")
    check("chrome label value excluded from descriptor",
          descriptor(None, n), "A:\n0:text@lbl")

    # Status bar -> promoted to value-role `status` (normalizes to node body).
    n = capture.node_from_attrs("STATUS_BAR", "bar", value="5")
    check("status bar promotion", (n.role, n.value), ("status", "5"))

    # Live region: a label with an active `live` attribute -> status.
    n = capture.node_from_attrs("LABEL", "live1", attrs={"live": "polite"}, value="7")
    check("live region -> status", (n.role, n.value), ("status", "7"))

    # Plain progress bar (no value) stays transient `progress`.
    n = capture.node_from_attrs("PROGRESS_BAR", None)
    check("progress no value stays progress", n.role, "progress")

    # Progress bar WITH a value -> value-role `progressbar`.
    n = capture.node_from_attrs("PROGRESS_BAR", "p", value="50")
    check("progressbar with value", (n.role, n.value), ("progressbar", "50"))

    # Empty accessible-id normalizes to None.
    n = capture.node_from_attrs("PUSH_BUTTON", "   ")
    check("blank id -> None", n.id, None)


def test_atk_tree_walk():
    # A synthetic GTK login window: frame > [heading, entry, password, button].
    tree = FakeAtk("FRAME", accessible_id="login", children=[
        FakeAtk("HEADING", name="Sign in"),
        FakeAtk("ENTRY", accessible_id="email", text="a@b.com"),
        FakeAtk("PASSWORD_TEXT", accessible_id="pwd", text="secret"),
        FakeAtk("PUSH_BUTTON", accessible_id="go", name="Log in"),
    ])
    root = capture.build_node_atk(tree)

    # The same logical tree built by hand against the canonical model.
    expected = Node(role="screen", id="login", children=[
        Node(role="header"),
        Node(role="textfield", id="email", value="a@b.com"),
        Node(role="textfield", id="pwd", type="password", value="secret"),
        Node(role="button", id="go"),
    ])
    check("ATK walk descriptor == hand-built descriptor",
          descriptor(capture.anchor_of(tree), root),
          descriptor("login", expected))


def test_atspi_tree_walk_matches_atk():
    # The SAME logical screen via the AT-SPI (Qt) walk must fold identically to
    # the ATK (GTK) walk: that is the cross-toolkit parity guarantee.
    atk_tree = FakeAtk("WINDOW", accessible_id="main", children=[
        FakeAtk("LABEL", name="Counter"),
        FakeAtk("STATUS_BAR", accessible_id="count", current_value=5),
    ])
    atspi_tree = FakeAtspi("WINDOW", accessible_id="main", children=[
        FakeAtspi("LABEL", name="Counter"),
        FakeAtspi("STATUS_BAR", accessible_id="count", current_value=5),
    ])
    d_atk = descriptor(capture.anchor_of(atk_tree), capture.build_node_atk(atk_tree))
    d_atspi = descriptor("main", capture.build_node_atspi(atspi_tree))
    check("GTK(ATK) and Qt(AT-SPI) walks fold identically", d_atk, d_atspi)
    # And the value-class lands in the V: section (status value 5 -> POS1).
    check("status value-class in V: section",
          d_atk.endswith("\nV:key:count=POS1"), True)


def test_transient_dropped_in_walk():
    # A spinner subtree must vanish from the descriptor (rule 2).
    tree = FakeAtk("FRAME", accessible_id="w", children=[
        FakeAtk("PUSH_BUTTON", accessible_id="ok"),
        FakeAtk("SPINNER", children=[FakeAtk("LABEL", name="Loading")]),
    ])
    root = capture.build_node_atk(tree)
    expected = Node(role="screen", id="w", children=[Node(role="button", id="ok")])
    check("spinner subtree dropped",
          descriptor(capture.anchor_of(tree), root), descriptor("w", expected))


def main():
    test_node_from_attrs()
    test_atk_tree_walk()
    test_atspi_tree_walk_matches_atk()
    test_transient_dropped_in_walk()

    if FAILS:
        print("FAIL: reproit-linux capture mapping")
        print("\nMismatches:")
        for f in FAILS:
            print(f)
        return 1
    print("PASS: reproit-linux capture mapping (node_from_attrs, ATK walk, "
          "AT-SPI walk, cross-toolkit parity, transient drop) with synthetic trees.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
