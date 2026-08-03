# /// script
# requires-python = ">=3.9"
# ///
"""A tiny deterministic curses TUI, used to validate reproit's tui backend.

Screens: Main -> {Play, Settings, Quit-confirm}. Arrows move the selection,
Enter selects, q goes back. Each screen renders distinct text, so the VT-grid
signature changes per screen and the explorer maps the navigation graph.

Run in a terminal:  python3 menu.py   (or: uv run menu.py)
"""

import curses

MENUS = {
    "MAIN": ["Play", "Settings", "Quit"],
    "SETTINGS": ["Toggle Sound", "Back"],
    "PLAY": ["Pause", "Quit to Menu"],
    "CONFIRM": ["Yes, quit", "No, stay"],
}
TITLES = {
    "MAIN": "Main Menu",
    "SETTINGS": "Settings",
    "PLAY": "Now Playing",
    "CONFIRM": "Really quit?",
}


def main(stdscr):
    curses.curs_set(0)
    screen, sel = "MAIN", 0
    while True:
        stdscr.clear()
        stdscr.addstr(1, 2, "=== %s ===" % TITLES[screen])
        for i, item in enumerate(MENUS[screen]):
            marker = "> " if i == sel else "  "
            stdscr.addstr(3 + i, 2, marker + item)
        stdscr.addstr(11, 2, "arrows: move   enter: select   q: back")
        stdscr.refresh()

        c = stdscr.getch()
        if c == curses.KEY_DOWN:
            sel = (sel + 1) % len(MENUS[screen])
        elif c == curses.KEY_UP:
            sel = (sel - 1) % len(MENUS[screen])
        elif c in (10, 13, curses.KEY_ENTER):
            choice = MENUS[screen][sel]
            if choice == "Play":
                screen, sel = "PLAY", 0
            elif choice == "Settings":
                screen, sel = "SETTINGS", 0
            elif choice == "Quit":
                screen, sel = "CONFIRM", 0
            elif choice in ("Back", "Quit to Menu", "No, stay"):
                screen, sel = "MAIN", 0
            elif choice == "Yes, quit":
                return
            # Pause / Toggle Sound: no-op, stay on screen
        elif c == ord("q"):
            if screen == "MAIN":
                return
            screen, sel = "MAIN", 0


if __name__ == "__main__":
    curses.wrapper(main)
