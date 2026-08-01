# Joplin iOS: the padded band around a note row is drawn but not touchable

Companion prose for `joplin-note-row-touch-target-15972.json`. This is the
first executed React Native iOS application campaign, run on disposable
`iPhone 16 Pro` simulators on iOS 26.2 through Appium 3.5.2 and the XCUITest
driver 11.16.2.

- Application: `laurent22/joplin`, `packages/app-mobile`, React Native 0.81.6.
- Issue: https://github.com/laurent22/joplin/issues/15972, fixed by pull
  request 15987.
- Affected `7d90db0bf68c7ea2803227f9e6277bb3cf697fb3`, fixed
  `2fa45a5a05daa597d52b73fce120e9242a6c6860`. The fix commit is a squash whose
  sole parent is the affected revision.
- Identity: `react-native-layout:note-row-padding-outside-touch-target`.

## Why this defect belongs to this target

`NoteItem.tsx` puts the row padding on the outer wrapper view rather than on
the pressable, so the padded band around a note title is painted but is not
part of the hit area. The fix moves `paddingLeft`, `paddingRight`,
`paddingTop` and `paddingBottom` onto the pressable.

A JavaScript-only harness cannot see this. The two builds render identically:
the same text at the same coordinates, the same row height, the same spacing.
The only thing that differs is which native view receives the touch. XCUITest
can see it, because it reports the note row as an `XCUIElementTypeButton` whose
frame **is** the pressable.

## The observable, measured before any run was spent

| revision | note row hit area |
| --- | --- |
| affected | `x=16 y=127 w=370 h=20` |
| fixed | `x=0 y=111 w=402 h=52` |

The hit area grows by exactly the 16pt padding on all four sides, and both
revisions centre the row identically at `(201, 137)`. A tap a fixed distance
above that centre is therefore the same absolute point on either build, which
is what makes one coordinate a fair trigger for both rather than two different
triggers compared against each other.

## Runs

Every run installs the ad-hoc signed simulator build onto a simulator created
for the campaign and reinstalls the application, so the Welcome notebook and
its five notes are genuinely first-launch content. Every run tapped
`(201, 118)`.

| run | revision | hit area | note opened | identity |
| --- | --- | --- | --- | --- |
| 1 | affected | 370x20 | no | the identity |
| 2 | affected | 370x20 | no | the identity |
| 3 | affected | 370x20 | no | the identity |
| 1 | fixed | 402x52 | yes | none |
| 2 | fixed | 402x52 | yes | none |
| 3 | fixed | 402x52 | yes | none |

Three affected reproductions, one exact identity, no drift. Three fixed
controls, every one reaching the same observation point and opening the note
from the identical coordinate.

## Minimized trigger

One tap. Not a gesture, not a sequence, not a scroll: a single tap at the row
centre offset 19pt upward, which is inside the painted row on both revisions
and inside the pressable only on the fixed one.

## Neighboring legal behavior

The same single tap moved to 9pt above the centre, which is inside the hit area
on both revisions, opens the note on both. That is the boundary the fix draws,
and it holds on the defective build, so the oracle is not simply reporting that
taps near a row edge sometimes fail.

## Reading the screen

The tapped row's title cannot say which screen is showing, because the opened
note repeats its own title. The note list's LAST row can, because it is on
screen only while the list is. Each run retains both facts, so a future reader
can see that the distinction was made deliberately.
