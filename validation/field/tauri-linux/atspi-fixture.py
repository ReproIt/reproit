#!/usr/bin/env python3
"""Fixture for the native-window channel.

A GTK 3 window with one button that opens a real GtkFileChooserDialog. The
channel is proven against this before any campaign depends on it: the fixture
prints the path the chooser returned, so a driven selection is confirmed by the
application under test rather than by the driver's own claim.
"""

import sys

import gi

gi.require_version("Gtk", "3.0")

from gi.repository import Gtk  # noqa: E402


class Fixture(Gtk.Window):
    def __init__(self) -> None:
        super().__init__(title="Reproit Native Channel Fixture")
        self.set_default_size(420, 160)
        button = Gtk.Button(label="Open File")
        button.connect("clicked", self.on_clicked)
        self.add(button)
        self.connect("destroy", Gtk.main_quit)

    def on_clicked(self, _button: Gtk.Button) -> None:
        dialog = Gtk.FileChooserDialog(
            title="Choose a fixture file",
            parent=self,
            action=Gtk.FileChooserAction.OPEN,
        )
        dialog.add_buttons("Cancel", Gtk.ResponseType.CANCEL, "Open", Gtk.ResponseType.OK)
        dialog.set_select_multiple(True)
        response = dialog.run()
        if response == Gtk.ResponseType.OK:
            for name in dialog.get_filenames():
                print(f"FIXTURE-SELECTED {name}", flush=True)
        else:
            print("FIXTURE-CANCELLED", flush=True)
        dialog.destroy()


def main() -> int:
    window = Fixture()
    window.show_all()
    print("FIXTURE-READY", flush=True)
    Gtk.main()
    return 0


if __name__ == "__main__":
    sys.exit(main())
