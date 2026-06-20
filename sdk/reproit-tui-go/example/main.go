// Command example is a minimal embed demo for the reproit-tui SDK. It is a tiny
// counter "TUI" (no real terminal control, just a rendered cell grid) that shows how
// an app feeds each rendered frame to the SDK and lets the SDK report sessions,
// coverage edges and crashes. A real bubbletea/tview app would build the grid from
// its own renderer in View()/Draw() and call Observe there.
//
// Run: go run ./example
//
// No em dashes anywhere, per project rules.
package main

import (
	"fmt"

	reproittui "github.com/reproit/reproit-tui"
)

// render builds a ScreenContents cell grid for a counter at n. In a real app you
// would translate your framework's buffer (bubbletea's view string, tview's screen
// cells, tcell's CellBuffer) into this grid; here we build it by hand.
func render(n int) reproittui.ScreenContents {
	line := fmt.Sprintf("Count: %d", n)
	rs := []rune(line)
	row := make([]reproittui.Cell, len(rs))
	for i, r := range rs {
		row[i] = reproittui.Cell{Contents: string(r)}
	}
	// cursor parked on the value column.
	return reproittui.ScreenContents{
		Grid:      [][]reproittui.Cell{row},
		CursorRow: 0,
		CursorCol: 7,
	}
}

func main() {
	// Endpoint left empty so the demo prints events locally instead of POSTing.
	r := reproittui.New(reproittui.Config{
		AppID: "counter-demo",
		Ctx:   map[string]interface{}{"version": "0.1.0"},
		OnEvent: func(e reproittui.Event) {
			fmt.Printf("[reproit] %+v\n", e)
		},
	})
	defer r.InstallCrashHandler()() // installs signal handler; returned fn recovers panics
	defer r.Flush()

	// Drive the "app": 0 -> 1 -> 2 -> 12. The SDK records an edge each time the
	// structural signature changes (0/1/2 are ZERO/POS1/POS1/POS2 buckets).
	for _, n := range []int{0, 1, 2, 12} {
		screen := render(n)
		r.Observe(screen, "key:Up")
		fmt.Printf("rendered count=%d -> sig=%s\n", n, r.CurrentSig())
	}
}
