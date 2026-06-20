"""The capture model: a ScreenContents the embedded app fills each frame, plus the
vt100 contents-string rendering the runner hashes.

This mirrors the Go SDK's ScreenContents (sdk/reproit-tui-go/reporter.go). It has
NO Textual/Rich/urwid import: it is a plain data model so the parity test runs on
any host. The app feeds it from whatever its framework gives it:

  - Textual / Rich: the app already renders to a console; Rich can EXPORT the
    rendered frame as text (Console(record=True).export_text(), or
    console.render_lines / Capture). Hand that text to ScreenContents.from_text
    (with the cursor cell) and the SDK signs the exact same screen the runner
    would. See README.md "Textual / Rich integration".
  - urwid / prompt_toolkit / a hand-rolled raw-mode dashboard: build the row-major
    cell grid with from_rows (a list of row strings, or a list of lists of cell
    graphemes) plus the cursor, and Text() reproduces vt100 screen().contents().

No em dashes anywhere, per project rules.
"""


class Cell:
    """One terminal cell. `contents` is the cell's grapheme (usually one
    character; "" for an empty/never-written cell). `wide` marks a double-width
    cell (CJK, some emoji): vt100 emits the wide cell's contents once and SKIPS the
    trailing spacer column, so Text() does too."""

    __slots__ = ("contents", "wide")

    def __init__(self, contents="", wide=False):
        self.contents = contents
        self.wide = wide


class ScreenContents:
    """The embed-API screen model: a row-major cell grid plus the cursor position,
    mirroring exactly what the runner's vt100 parser exposes (a rows x cols grid +
    screen().cursor_position()).

    Construct it one of three ways:
      - ScreenContents(grid=[[Cell,...],...], cursor=(row, col)) for a cell grid;
      - ScreenContents.from_rows([...], cursor=(row, col)) for row strings/lists;
      - ScreenContents.from_text("full\\nframe\\ntext", cursor=(row, col)) when you
        already hold the rendered contents string (Rich export, your own capture).
    """

    __slots__ = ("grid", "cursor", "raw")

    def __init__(self, grid=None, cursor=(0, 0), raw=None):
        self.grid = grid
        # cursor is a (row, col) tuple of 0-based ints (vt100 cursor_position()).
        self.cursor = (int(cursor[0]), int(cursor[1]))
        # When raw is not None it is used verbatim and grid is ignored.
        self.raw = raw

    @staticmethod
    def from_text(text, cursor=(0, 0)):
        """Build from a pre-rendered contents string (the Rich/Textual export path,
        or your own captured frame). The string is used verbatim by Text(); no
        per-row trimming is applied here because the producer (the runner's vt100
        parser, or Rich's export) already trimmed it the same way. Pass the cursor
        cell so focused-field changes register as distinct states."""
        return ScreenContents(raw=text if text is not None else "", cursor=cursor)

    @staticmethod
    def from_rows(rows, cursor=(0, 0)):
        """Build a grid from `rows`, where each row is either a string (one
        character per cell, "" not allowed mid-row) or a list of cell graphemes /
        Cell objects. A list-of-strings is the common path for urwid /
        prompt_toolkit canvases. Trailing-cell and trailing-row trimming happen in
        Text(), matching vt100 screen().contents()."""
        grid = []
        for row in rows:
            cells = []
            if isinstance(row, str):
                for ch in row:
                    cells.append(Cell(ch))
            else:
                for c in row:
                    if isinstance(c, Cell):
                        cells.append(c)
                    else:
                        cells.append(Cell("" if c is None else str(c)))
            grid.append(cells)
        return ScreenContents(grid=grid, cursor=cursor)

    def text(self):
        """Render the grid to the SAME contents string vt100's screen().contents()
        produces, byte-for-byte (the model the runner hashes), so the embedded
        signature equals the runner signature. Rules, taken from vt100 grid/row
        write_contents (mirrored from the Go SDK's ScreenContents.Text):

          - For each row, walk cells left to right. Emit a space for each gap
            column that PRECEDES a non-empty cell; emit the cell's contents for a
            non-empty cell. A wide cell emits its contents and the next column (its
            spacer) is skipped. Trailing empty cells emit NOTHING (per-row trailing
            whitespace is trimmed).
          - Rows are joined with '\\n'.
          - Finally, all trailing '\\n' are stripped (trailing blank rows trimmed).
        """
        if self.raw is not None or self.grid is None:
            return self.raw if self.raw is not None else ""
        parts = []
        for row in self.grid:
            # find the last non-empty cell so trailing empties are dropped
            last = -1
            for i, c in enumerate(row):
                if c.contents != "":
                    last = i
            row_out = []
            col = 0
            while col <= last:
                c = row[col]
                if c.contents != "":
                    row_out.append(c.contents)
                    if c.wide:
                        col += 2  # skip the spacer column the wide glyph occupies
                        continue
                else:
                    row_out.append(" ")  # a gap before a later non-empty cell
                col += 1
            parts.append("".join(row_out))
        out = "\n".join(parts)
        # strip trailing blank rows (all trailing '\n')
        return out.rstrip("\n")
