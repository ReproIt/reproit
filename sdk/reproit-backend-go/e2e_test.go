// Functional end-to-end test: a real net/http server with a planted 500,
// real HTTP requests, and a local stub ingest server. Asserts the finding
// batch arrives correctly tagged with the reproitCapture sequence, and that
// a scan-time request round-trips the x-reproit-events header.
package reproitbackend

import (
	"bytes"
	"encoding/base64"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
	"time"
)

type stubIngest struct {
	mu       sync.Mutex
	auth     []string
	batches  []map[string]any
	received chan struct{}
}

func (s *stubIngest) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	body, _ := io.ReadAll(r.Body)
	var batch map[string]any
	if err := json.Unmarshal(body, &batch); err != nil {
		http.Error(w, "bad batch", http.StatusBadRequest)
		return
	}
	s.mu.Lock()
	s.auth = append(s.auth, r.Header.Get("Authorization"))
	s.batches = append(s.batches, batch)
	s.mu.Unlock()
	select {
	case s.received <- struct{}{}:
	default:
	}
	w.Header().Set("Content-Type", "application/json")
	_, _ = w.Write([]byte(`{"accepted":true}`))
}

func testApp(capture *Capture) http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /ok", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"ok":true}`))
	})
	mux.HandleFunc("POST /boom", func(w http.ResponseWriter, r *http.Request) {
		if trace := FromRequest(r); trace != nil {
			_ = trace.Effect(EffectWrite, EffectOptions{Resource: "orders", Key: "1"})
		}
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusInternalServerError)
		_, _ = w.Write([]byte(`{"error":"boom"}`))
	})
	middleware := Middleware(MiddlewareOptions{Capture: capture})
	return middleware(mux)
}

func TestE2EPlanted500ShipsATaggedFindingBatch(t *testing.T) {
	ingest := &stubIngest{received: make(chan struct{}, 1)}
	ingestServer := httptest.NewServer(ingest)
	defer ingestServer.Close()

	config := NewCaptureConfig(ingestServer.URL+"/v1/events", "sk_live_test", "app-e2e")
	config.Build = "9.9.9"
	config.FlushInterval = 100 * time.Millisecond
	capture := NewCapture(config)
	if capture == nil {
		t.Fatal("capture must start")
	}

	app := httptest.NewServer(testApp(capture))
	defer app.Close()

	body := bytes.NewReader([]byte(`{"item":"widget","apiKey":"sk_live_leak"}`))
	boom, err := http.Post(app.URL+"/boom", "application/json", body)
	if err != nil {
		t.Fatal(err)
	}
	boom.Body.Close()
	if boom.StatusCode != http.StatusInternalServerError {
		t.Fatalf("planted bug returned %d", boom.StatusCode)
	}
	if !capture.Flush(5 * time.Second) {
		t.Fatal("flush timed out")
	}

	ingest.mu.Lock()
	defer ingest.mu.Unlock()
	if len(ingest.batches) != 1 {
		t.Fatalf("expected 1 ingest batch, got %d", len(ingest.batches))
	}
	if ingest.auth[0] != "Bearer sk_live_test" {
		t.Fatalf("wrong authorization: %q", ingest.auth[0])
	}
	batch := ingest.batches[0]
	if batch["projectId"] != "app-e2e" {
		t.Fatalf("wrong projectId: %v", batch["projectId"])
	}
	if batch["deployment"].(map[string]any)["version"] != "9.9.9" {
		t.Fatal("deployment version lost")
	}
	var finding map[string]any
	for _, item := range batch["events"].([]any) {
		event := item.(map[string]any)["event"].(map[string]any)
		if event["kind"] == "observation" {
			if finding != nil {
				t.Fatal("expected exactly one observation")
			}
			finding = event
		}
	}
	if finding == nil {
		t.Fatal("no observation in the batch")
	}
	if finding["failure"].(map[string]any)["signature"] !=
		ServerErrorOracle+":POST /boom" {
		t.Fatal("observation not tagged backend-server-error")
	}
	events := batch["events"].([]any)
	kinds := make([]string, 0, len(events))
	for _, item := range events {
		kinds = append(
			kinds,
			item.(map[string]any)["event"].(map[string]any)["kind"].(string),
		)
	}
	if len(kinds) != 5 || kinds[0] != "operation-start" ||
		kinds[1] != "trigger" || kinds[2] != "effect" ||
		kinds[3] != "operation-end" || kinds[4] != "observation" {
		t.Fatalf("capture sequence wrong: %v", kinds)
	}
	effect := events[2].(map[string]any)["event"].(map[string]any)
	if effect["subject"] != "orders" {
		t.Fatalf("effect resource wrong: %v", effect)
	}
	// The secret-shaped input field was structurally redacted before upload.
	trigger := events[1].(map[string]any)["event"].(map[string]any)
	inputBody := trigger["value"].(map[string]any)["value"].(map[string]any)["body"].(map[string]any)
	stub := inputBody["apiKey"].(map[string]any)["$reproit"].(map[string]any)
	if stub["redacted"] != true {
		t.Fatal("apiKey shipped unredacted")
	}
	if inputBody["item"] != "widget" {
		t.Fatal("non-secret input field damaged")
	}

	// Scan-time round-trip: x-reproit-trace in, x-reproit-events out.
	request, _ := http.NewRequest(http.MethodGet, app.URL+"/ok", nil)
	request.Header.Set("x-reproit-trace", "trace-e2e")
	request.Header.Set("x-reproit-actor", "alice")
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	response.Body.Close()
	header := response.Header.Get("x-reproit-events")
	if header == "" {
		t.Fatal("expected an x-reproit-events response header")
	}
	decoded, err := base64.RawURLEncoding.DecodeString(header)
	if err != nil {
		t.Fatal(err)
	}
	var traceEvents []map[string]any
	if err := json.Unmarshal(decoded, &traceEvents); err != nil {
		t.Fatal(err)
	}
	first := traceEvents[0]
	if first["traceId"] != "trace-e2e" || first["actor"] != "alice" {
		t.Fatalf("scan trace context wrong: %v", first)
	}
	last := traceEvents[len(traceEvents)-1]
	if last["kind"] != "return" || last["status"].(float64) != 200 {
		t.Fatalf("scan return event wrong: %v", last)
	}
	// The healthy scan-time request must not have been captured.
	if stats := capture.Stats(); stats.CapturedOperations != 1 {
		t.Fatalf("captured %d operations, want 1", stats.CapturedOperations)
	}
}
