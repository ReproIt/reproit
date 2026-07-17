// screen.ts is the capture model: the embed-API screen representation the app
// hands the SDK each rendered frame. It mirrors the Go SDK's ScreenContents and
// reproduces the runner's vt100 screen().contents() text model.
//
// Two construction shapes, matching whatever the app already holds:
//
//   ScreenContents.fromText(text, cursor)   -- the app already has a rendered
//       contents string (Ink renders its component tree to a string internally;
//       hand that string straight in). Lowest-friction path for Ink and any
//       framework that gives you a frame string.
//
//   ScreenContents.fromRows(rows, cursor)   -- the app hands a row-major cell
//       grid (tcell/blessed-style buffers, or a framework cell buffer); the SDK
//       renders it to the SAME contents string vt100's screen().contents()
//       produces, byte-for-byte, so the reported signature equals the runner's.
//
// No em dashes anywhere, per project rules.

// Cursor is the 0-based [row, col] cursor cell, matching vt100
// screen().cursor_position() (which the canonical crate reads as the (row, col)
// tuple). Same convention as the Go SDK CursorRow/CursorCol.
export type Cursor = [number, number];

// Cell is one terminal cell. `contents` is the cell's grapheme (usually one code
// point; "" for an empty/never-written cell). `wide` marks a double-width cell
// (CJK, some emoji): vt100 emits the wide cell's contents once and SKIPS the
// trailing spacer column, so the renderer must too (see fromRows).
export interface Cell {
  contents: string;
  wide?: boolean;
}

// A grid row is an array of cells (or, for convenience, an array of single-char
// strings, treated as non-wide cells). Rows need not be the same length; missing
// trailing cells are treated as empty, exactly like vt100 trailing-space trimming.
export type Row = Array<Cell | string>;

// ScreenContents holds the rendered contents string plus the cursor cell. Build it
// with fromText (you have a string) or fromRows (you have a cell grid).
export class ScreenContents {
  readonly text: string;
  readonly cursorRow: number;
  readonly cursorCol: number;

  private constructor(text: string, cursorRow: number, cursorCol: number) {
    this.text = text;
    this.cursorRow = cursorRow >>> 0;
    this.cursorCol = cursorCol >>> 0;
  }

  // fromText wraps an already-rendered contents string verbatim. This is the Ink
  // path: Ink renders its component tree to a single string each frame, so pass
  // that string here. cursor defaults to [0, 0] when the framework does not
  // surface a cursor position.
  static fromText(text: string, cursor: Cursor = [0, 0]): ScreenContents {
    return new ScreenContents(text ?? '', cursor[0], cursor[1]);
  }

  // fromRows renders a row-major cell grid to the SAME contents string vt100's
  // screen().contents() produces, byte-for-byte. The rules, taken from vt100-0.15
  // grid/row write_contents (the same rules the Go SDK's ScreenContents.Text uses):
  //
  //   - For each row, walk cells left to right. Emit a space for each gap column
  //     that precedes a non-empty cell; emit the cell's contents for a non-empty
  //     cell. A wide cell emits its contents and the next column (its spacer) is
  //     skipped. Trailing empty cells emit NOTHING (per-row trailing whitespace is
  //     trimmed).
  //   - Rows are joined with '\n'.
  //   - Finally, all trailing '\n' are stripped (trailing blank rows are trimmed).
  //
  // This is the model the runner hashes, so reproducing it exactly is what makes
  // the embedded signature equal the runner signature.
  static fromRows(rows: Row[], cursor: Cursor = [0, 0]): ScreenContents {
    let out = '';
    for (const row of rows) {
      // normalize each cell to {contents, wide}
      const cells: Cell[] = row.map((c) => (typeof c === 'string' ? { contents: c } : c));
      // find the last non-empty cell so trailing empties are dropped
      let last = -1;
      for (let i = 0; i < cells.length; i++) {
        if (cells[i].contents !== '') last = i;
      }
      let col = 0;
      while (col <= last) {
        const c = cells[col];
        if (c.contents !== '') {
          out += c.contents;
          if (c.wide) {
            col += 2; // skip the spacer column the wide glyph occupies
            continue;
          }
        } else {
          out += ' '; // a gap before a later non-empty cell
        }
        col++;
      }
      out += '\n';
    }
    // strip all trailing newlines (trailing blank rows trimmed)
    out = out.replace(/\n+$/, '');
    return new ScreenContents(out, cursor[0], cursor[1]);
  }
}
