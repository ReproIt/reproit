package reproittui

// Tests for the embeddable reporter half: event contract, edge-on-signature-change,
// batch shape ({appId, sentAt, ctx?, events}), and crash flush.

import (
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
)

func TestObserveEmitsEdgeOnlyOnSignatureChange(t *testing.T) {
	var mu sync.Mutex
	var seen []Event
	r := New(Config{AppID: "demo", OnEvent: func(e Event) {
		mu.Lock()
		seen = append(seen, e)
		mu.Unlock()
	}})

	// session event on New + first observe opens an edge from "".
	r.ObserveContents("Count: 0\n", 0, 8, "key:Enter")
	// same screen again -> no edge (signature unchanged).
	r.ObserveContents("Count: 0\n", 0, 8, "key:Down")
	// value tick to a DIFFERENT bucket (0 -> 1) -> a new signature -> an edge.
	r.ObserveContents("Count: 1\n", 0, 8, "key:Up")

	mu.Lock()
	defer mu.Unlock()
	var sessions, edges int
	for _, e := range seen {
		switch e.Kind {
		case "session":
			sessions++
		case "edge":
			edges++
		}
	}
	if sessions != 1 {
		t.Errorf("want exactly 1 session event, got %d", sessions)
	}
	if edges != 2 {
		t.Errorf("want 2 edge events (init + bucket change), got %d (%+v)", edges, seen)
	}
	// the first edge's `to` must equal the count0 golden signature.
	if r.CurrentSig() != "63430c00" {
		t.Errorf("CurrentSig after count1 = %s, want 63430c00 (count1 golden)", r.CurrentSig())
	}
}

func TestFlushPostsCanonicalBatchContract(t *testing.T) {
	var gotBody []byte
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		if req.Method != http.MethodPost {
			t.Errorf("want POST, got %s", req.Method)
		}
		if ct := req.Header.Get("Content-Type"); ct != "application/json" {
			t.Errorf("want application/json, got %s", ct)
		}
		gotBody, _ = io.ReadAll(req.Body)
		w.WriteHeader(http.StatusOK)
	}))
	defer srv.Close()

	r := New(Config{AppID: "myapp", Endpoint: srv.URL, Ctx: map[string]interface{}{"v": "1.0"}})
	r.ObserveContents("Count: 0\n", 0, 8, "key:Enter")
	r.Flush()

	var b batch
	if err := json.Unmarshal(gotBody, &b); err != nil {
		t.Fatalf("server did not receive valid JSON batch: %v (%s)", err, gotBody)
	}
	if b.AppID != "myapp" {
		t.Errorf("batch.appId = %q, want myapp", b.AppID)
	}
	if b.SentAt == 0 {
		t.Errorf("batch.sentAt must be set")
	}
	if b.Ctx["v"] != "1.0" {
		t.Errorf("batch.ctx not forwarded: %+v", b.Ctx)
	}
	if len(b.Events) == 0 {
		t.Errorf("batch.events must be non-empty")
	}
}

func TestReportCrashFlushesWithSignature(t *testing.T) {
	var mu sync.Mutex
	var crash *Event
	r := New(Config{AppID: "demo", OnEvent: func(e Event) {
		if e.Kind == "crash" {
			mu.Lock()
			c := e
			crash = &c
			mu.Unlock()
		}
	}})
	r.ObserveContents("Count: 12\n", 0, 8, "key:Enter")
	r.ReportCrash("boom")

	mu.Lock()
	defer mu.Unlock()
	if crash == nil {
		t.Fatal("no crash event emitted")
	}
	if crash.Error != "boom" {
		t.Errorf("crash.error = %q, want boom", crash.Error)
	}
	if crash.Sig != "664310b9" { // count12 golden sig
		t.Errorf("crash carried sig %q, want the current state sig 664310b9", crash.Sig)
	}
}
