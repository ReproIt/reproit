#!/usr/bin/env python3
"""Press the fixture's button through its own accessible action.

Part of the native-window channel proof: the click is delivered by the toolkit's
action interface, not by synthetic X11 input, so a subject that exposes no
action is reported rather than driven blind.
"""

import sys

sys.path.insert(0, "/field")

from atspi_window import _action_names, _first, find_window  # noqa: E402

window, waited = find_window("Reproit Native Channel Fixture", 30)
if window is None:
    raise SystemExit("the fixture window never reached the accessibility bus")
button = _first(window, lambda node: node.getRoleName() == "push button")
if button is None:
    raise SystemExit("the fixture window exposes no push button")
print("BUTTON", button.name, _action_names(button), "waited", waited)
button.queryAction().doAction(0)
