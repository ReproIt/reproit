#!/usr/bin/env python3
"""GTK3 fixture app for the reproit linux-atspi harness.

A tiny multi-screen GTK3 app that publishes a clean AT-SPI tree with
stable accessible-ids on every interactive control, so the canonical
structural signature is well defined and the runner can drive it.

Screens (a GtkStack, one signature per visible page):
  - home:     Open Settings / Open Help buttons
  - settings: a textfield + Back button
  - help:     a Back button, AND a "Get Stuck" button that navigates to
              a dead-end page

Planted bug (non-crash oracle, a stuck / dead-end state):
  - The "dead-end" page has NO way back. Its only control is a disabled,
    non-actionable button. Once reproit taps "Get Stuck" it can never
    leave: a terminal sink that is not an intended terminal state. This
    is exactly the `no-dead-end` invariant violation, surfaced as a
    stuck state rather than a process crash, so the harness stays clean.

Every actionable widget gets a stable accessible-id via
Atk.Object.set_accessible_id-equivalent: GTK3 exposes this through
`set_name` on the AtkObject only for the *name* (localized), so for a
stable, language-independent id we set the GTK widget name AND the
accessible-id when the running atk bridge supports it. GTK3's atk-bridge
maps `gtk_widget_set_name` to the AT-SPI object's accessible-id on recent
at-spi2 stacks (get_accessible_id), which is what the runner reads.
"""

import gi
gi.require_version("Gtk", "3.0")
from gi.repository import Gtk, GLib


def set_aid(widget, aid):
    """Give a widget a stable, language-independent accessible id.

    The runner reads Atspi.get_accessible_id(); on the GTK3 side that is
    backed by Atk.Object.set_accessible_id (present on GTK 3.24.41). This
    is the developer key the canonical signature hashes, so the same screen
    in any locale produces the same structural signature.
    """
    ax = widget.get_accessible()
    if ax is not None and hasattr(ax, "set_accessible_id"):
        ax.set_accessible_id(aid)
    else:
        widget.set_name(aid)  # fallback (older ATK)


class FixtureWindow(Gtk.Window):
    def __init__(self):
        super().__init__(title="ReproIt AT-SPI Fixture")
        self.set_default_size(420, 320)
        set_aid(self, "fixture-window")

        self.stack = Gtk.Stack()
        self.stack.set_transition_type(Gtk.StackTransitionType.NONE)
        self.add(self.stack)

        self.stack.add_named(self._home(), "home")
        self.stack.add_named(self._settings(), "settings")
        self.stack.add_named(self._help(), "help")
        self.stack.add_named(self._deadend(), "deadend")

        self.stack.set_visible_child_name("home")

    def _go(self, name):
        def handler(_btn):
            self.stack.set_visible_child_name(name)
        return handler

    def _home(self):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
        box.set_border_width(24)
        set_aid(box, "home-page")

        title = Gtk.Label(label="Home")
        set_aid(title, "home-title")
        box.pack_start(title, False, False, 0)

        b_settings = Gtk.Button(label="Open Settings")
        set_aid(b_settings, "open-settings")
        b_settings.connect("clicked", self._go("settings"))
        box.pack_start(b_settings, False, False, 0)

        b_help = Gtk.Button(label="Open Help")
        set_aid(b_help, "open-help")
        b_help.connect("clicked", self._go("help"))
        box.pack_start(b_help, False, False, 0)

        return box

    def _settings(self):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
        box.set_border_width(24)
        set_aid(box, "settings-page")

        title = Gtk.Label(label="Settings")
        set_aid(title, "settings-title")
        box.pack_start(title, False, False, 0)

        entry = Gtk.Entry()
        entry.set_placeholder_text("Your name")
        set_aid(entry, "name-field")
        box.pack_start(entry, False, False, 0)

        back = Gtk.Button(label="Back")
        set_aid(back, "settings-back")
        back.connect("clicked", self._go("home"))
        box.pack_start(back, False, False, 0)

        return box

    def _help(self):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
        box.set_border_width(24)
        set_aid(box, "help-page")

        title = Gtk.Label(label="Help")
        set_aid(title, "help-title")
        box.pack_start(title, False, False, 0)

        back = Gtk.Button(label="Back")
        set_aid(back, "help-back")
        back.connect("clicked", self._go("home"))
        box.pack_start(back, False, False, 0)

        # PLANTED BUG: navigates to a dead-end with no way out.
        stuck = Gtk.Button(label="Get Stuck")
        set_aid(stuck, "get-stuck")
        stuck.connect("clicked", self._go("deadend"))
        box.pack_start(stuck, False, False, 0)

        return box

    def _deadend(self):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
        box.set_border_width(24)
        set_aid(box, "deadend-page")

        title = Gtk.Label(label="Dead End")
        set_aid(title, "deadend-title")
        box.pack_start(title, False, False, 0)

        # The only control is disabled and non-actionable: no escape.
        trap = Gtk.Button(label="Nothing here")
        set_aid(trap, "deadend-trap")
        trap.set_sensitive(False)
        box.pack_start(trap, False, False, 0)

        return box


def main():
    # The runner finds the app by AT-SPI *application* name (substring match).
    # A PyGObject app otherwise registers as "python3"; name it so
    # REPROIT_TARGET=Fixture resolves.
    GLib.set_prgname("Fixture")
    GLib.set_application_name("Fixture")
    win = FixtureWindow()
    win.connect("destroy", Gtk.main_quit)
    win.show_all()
    # Auto-quit after a generous window so the container never hangs if the
    # runner crashes; the runner finishes well before this.
    GLib.timeout_add_seconds(120, Gtk.main_quit)
    Gtk.main()


if __name__ == "__main__":
    main()
