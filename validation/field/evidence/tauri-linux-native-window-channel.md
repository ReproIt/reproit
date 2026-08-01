# tauri-linux: the native-window channel, proven on a fixture

A Tauri application is not only its webview. File choosers, message dialogs, and
menus are native GTK windows, and WebDriver cannot see them at all: a run that
opens one looks, through the webview channel, like a run where nothing happened.
That is why readest and note-gen were previously recorded as unusable. They are
not unusable; the harness had one channel and needed two.

This is the second channel: `validation/field/tauri-linux/atspi_window.py`,
reading and driving native windows over AT-SPI in the same worker, with the
webview channel unchanged and still primary.

## Two properties the channel enforces

1. **A single desktop read is not a measurement.** An application registers with
   the accessibility bus asynchronously, so one traversal straight after a click
   finds an empty desktop and makes a live toolkit look invisible. Every lookup
   polls to a deadline and reports how long it waited.
2. **The channel never invents an interaction.** Buttons are pressed through the
   accessible action and text goes in through `EditableText.setTextContents`, so
   a subject exposing neither is reported as unreachable rather than driven by
   synthetic X11 keystrokes that no accessible tree can confirm.

## Fixture proof

`validation/field/tauri-linux/prove-native-channel.sh` runs a GTK 3 fixture with
one button that opens a real `GtkFileChooserDialog`, and requires the FIXTURE to
confirm the outcome on its own stdout. A driver that claims success without the
application agreeing is not a channel, so the fixture's line is the pass
condition, not the driver's return value.

```
=== windows on the bus
[{ "application": "atspi-fixture.py", "name": "Reproit Native Channel Fixture",
   "role": "frame", "children": 1 }]
=== press the fixture button through its accessible action
BUTTON Open File ['click'] waited 0.0
=== drive the chooser
{"found": true, "typed": true, "accepted": true, "entryRole": "text",
 "acceptedBy": "entry-activate", "text": "/tmp/pick/chosen.txt"}
=== fixture stdout
FIXTURE-READY
FIXTURE-SELECTED /tmp/pick/chosen.txt
native-window channel: PASS
```

Two things had to be right, and both took a measurement to find:

- GTK hides the chooser's location entry until it is asked for it. AT-SPI
  exposes that as the file-chooser widget's own `show_location` action, which is
  the accessible equivalent of Ctrl+L; the channel uses it rather than synthetic
  keys.
- The chooser holds three editable text nodes: the location entry and two
  typeahead entries, all role `text` with an `activate` action. Only the
  location entry sits under the filler GTK names `Location Layer`. Typing into
  either of the others silently does nothing, which is exactly the shape of a
  run that looks driven and is not. The channel matches the parent and falls
  back to the focused showing entry, never to document order.

## Proven on the real subject, not only the fixture

readest's library import is a native chooser, reached from the webview through
Import Books then From Local File. With the channel:

- `IMPORTED 14 of 14` books, each imported by driving the real GTK chooser, with
  the count read back from readest's own bookshelf in the webview.
- The books are generated EPUBs (`make-epubs.py`), distinct titles and
  identifiers, so a library that deduplicates still shows fourteen. Plain `.txt`
  files are rejected by the chooser's filter, which is why the first attempts
  imported nothing.
- Multiple quoted paths in one location entry select nothing here, so the
  campaign imports one book per dialog.

So the earlier disqualification is withdrawn: readest is reachable.

## What is still missing for the readest campaign

Two concrete inputs, both measured rather than guessed:

1. **A selection observable.** In list view with select mode on, the fixed build
   keeps the last row above the action bar, which is visible in the retained
   screenshot. The row's own `className` and computed `background-color` do not
   change when a row is selected, so neither can serve as the observable. The
   scenario needs the element readest actually marks, and until that is
   identified no run can be attributed.
2. **A reliable frontend build.** The Next.js export peaks above what the 8 GB
   Docker VM gives it: `next build` is SIGKILLed and pnpm reports only "Command
   was killed with SIGKILL". `NODE_OPTIONS=--max-old-space-size=3072` made the
   affected revision build once, with nothing else running, and the fixed
   revision was killed again while a scenario ran concurrently. The affected
   binary is staged; the fixed one is not.

The second is why this round produced no readest campaign. A binary built for
one revision and labelled with another is precisely the false pass this program
exists to prevent, so the mislabelled artifact was deleted rather than used.
