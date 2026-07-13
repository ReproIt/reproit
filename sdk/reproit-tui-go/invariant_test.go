package reproittui

// Dogfood the app-invariant oracle both directions: a violating state appends a
// REPROIT_INVARIANT marker to the runner-provisioned file (which the TUI backend
// re-emits as EXPLORE:INVARIANT), a clean state and a production run (no gate)
// append nothing.

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func markerItems(t *testing.T, path string) []map[string]any {
	t.Helper()
	b, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	var out []map[string]any
	for _, ln := range strings.Split(string(b), "\n") {
		rest, ok := strings.CutPrefix(ln, "REPROIT_INVARIANT ")
		if !ok {
			continue
		}
		var v map[string]any
		if err := json.Unmarshal([]byte(rest), &v); err != nil {
			t.Fatalf("bad marker json: %v", err)
		}
		out = append(out, v)
	}
	return out
}

func TestInvariantReportsOnlyViolationsUnderTheFuzzer(t *testing.T) {
	path := filepath.Join(t.TempDir(), "inv.ndjson")
	t.Setenv("REPROIT_INVARIANT_FILE", path)

	r := New(Config{AppID: "demo"})
	r.Invariant("holds", func() error { return nil })
	r.Invariant("neg", func() error { return errors.New("count < 0") })
	r.Invariant("boom", func() error { panic("kaboom") })

	r.ObserveContents("Count: -1", 0, 0, "key:Down")

	markers := markerItems(t, path)
	if len(markers) != 1 {
		t.Fatalf("want one marker, got %d: %v", len(markers), markers)
	}
	if markers[0]["sig"] != r.CurrentSig() {
		t.Fatalf("marker sig %v != current sig %v", markers[0]["sig"], r.CurrentSig())
	}
	items := markers[0]["items"].([]any)
	got := map[string]string{}
	for _, it := range items {
		m := it.(map[string]any)
		got[m["id"].(string)] = m["message"].(string)
	}
	if len(got) != 2 || got["neg"] != "count < 0" || got["boom"] != "kaboom" {
		t.Fatalf("unexpected violations: %v", got)
	}
	if _, ok := got["holds"]; ok {
		t.Fatalf("a holding invariant must not be reported")
	}
}

func TestInvariantSilentWhenCleanOrUngated(t *testing.T) {
	path := filepath.Join(t.TempDir(), "inv.ndjson")
	t.Setenv("REPROIT_INVARIANT_FILE", path)

	r := New(Config{AppID: "demo"})
	r.Invariant("holds", func() error { return nil })
	r.ObserveContents("Count: 3", 0, 0, "load")
	if got := markerItems(t, path); got != nil {
		t.Fatalf("a satisfied registry must write nothing, got %v", got)
	}

	// Inert without the gate (production).
	os.Unsetenv("REPROIT_INVARIANT_FILE")
	r2 := New(Config{AppID: "demo"})
	r2.Invariant("violated", func() error { return errors.New("bad") })
	r2.ObserveContents("Count: 4", 0, 0, "load")
}
