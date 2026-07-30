package reproitbackend

import (
	"encoding/json"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

const replayCapture = `{
  "format": "reproit-backend-capture",
  "version": 2,
  "operation": "GET /quote",
  "oracle": "backend-server-error",
  "envelope": {
    "observedAtMs": 1753747200000,
    "tz": "Europe/Berlin",
    "runtime": "go",
    "replaySeed": "00ff00ff00ff00ff"
  },
  "events": [
    {"kind": "start", "sequence": 1, "operation": "GET /quote"},
    {"kind": "effect", "sequence": 2, "effect": "read", "resource": "pg",
     "exchange": {"protocol": "pg",
       "request": {"text": "SELECT id FROM issuers WHERE symbol = $1", "values": ["ACME"]},
       "response": {"command": "SELECT", "rowCount": 1, "rows": [{"id": 7}]}}},
    {"kind": "effect", "sequence": 3, "effect": "call", "resource": "pricing",
     "exchange": {"protocol": "http",
       "request": {"method": "GET", "url": "http://pricing.internal/prices?tier=gold"},
       "response": {"status": 200, "headers": {"content-type": "application/json"},
                    "body": {"prices": null}}}},
    {"kind": "return", "sequence": 4, "status": 500, "success": false,
     "effectsComplete": true}
  ]
}`

func loadedCapture(t *testing.T, payload string) *replaySession {
	t.Helper()
	path := filepath.Join(t.TempDir(), "capture.json")
	if err := os.WriteFile(path, []byte(payload), 0o600); err != nil {
		t.Fatal(err)
	}
	loaded := loadReplaySession(path)
	if loaded == nil {
		t.Fatal("capture payload did not load")
	}
	return loaded
}

func TestReplayLoadsExchangesAndEnvelope(t *testing.T) {
	loaded := loadedCapture(t, replayCapture)
	if len(loaded.exchanges) != 2 {
		t.Fatalf("expected two exchanges, got %d", len(loaded.exchanges))
	}
	if loaded.envelope["tz"] != "Europe/Berlin" {
		t.Fatalf("envelope lost: %v", loaded.envelope)
	}
}

func TestReplayRejectsForeignOrUnsupportedPayloads(t *testing.T) {
	path := filepath.Join(t.TempDir(), "foreign.json")
	if err := os.WriteFile(path, []byte(`{"format":"something-else"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	if loadReplaySession(path) != nil {
		t.Fatal("a foreign payload was accepted as a capture")
	}
	future := filepath.Join(t.TempDir(), "future.json")
	payload := `{"format":"reproit-backend-capture","version":99,"events":[]}`
	if err := os.WriteFile(future, []byte(payload), 0o600); err != nil {
		t.Fatal(err)
	}
	if loadReplaySession(future) != nil {
		t.Fatal("an unsupported capture version was accepted")
	}
}

func TestReplayServesRecordedDatabaseRowsWithoutADatabase(t *testing.T) {
	loaded := loadedCapture(t, replayCapture)
	outcome, err := loaded.serveDB("SELECT id FROM issuers WHERE symbol = $1",
		[]any{"ACME"})
	if err != nil {
		t.Fatal(err)
	}
	if outcome.Command != "SELECT" || outcome.RowCount != 1 || len(outcome.Rows) != 1 {
		t.Fatalf("recorded outcome lost: %+v", outcome)
	}
}

func TestReplayServesRecordedHTTPResponseInProcess(t *testing.T) {
	loaded := loadedCapture(t, replayCapture)
	// Consume the pg exchange first: matching is strictly ordinal per
	// protocol, but protocols are independent queues.
	request, err := http.NewRequest(http.MethodGet,
		"http://pricing.internal/prices?tier=gold", nil)
	if err != nil {
		t.Fatal(err)
	}
	response, err := loaded.serveHTTP(request)
	if err != nil {
		t.Fatal(err)
	}
	if response.StatusCode != 200 {
		t.Fatalf("recorded status lost: %d", response.StatusCode)
	}
	body, err := io.ReadAll(response.Body)
	if err != nil {
		t.Fatal(err)
	}
	var decoded map[string]any
	if json.Unmarshal(body, &decoded) != nil {
		t.Fatalf("served body is not the recorded JSON: %s", body)
	}
	if value, present := decoded["prices"]; !present || value != nil {
		t.Fatalf("recorded body lost: %s", body)
	}
}

func TestUnmatchedCallDivergesWithTheStructuredMarker(t *testing.T) {
	loaded := loadedCapture(t, replayCapture)
	reader, writer, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	original := os.Stderr
	os.Stderr = writer
	request, err := http.NewRequest(http.MethodGet,
		"http://pricing.internal/unknown-endpoint", nil)
	if err != nil {
		os.Stderr = original
		t.Fatal(err)
	}
	response, err := loaded.serveHTTP(request)
	os.Stderr = original
	_ = writer.Close()
	if err != nil {
		t.Fatal(err)
	}
	if response.StatusCode != 599 {
		t.Fatalf("divergence did not fail closed: %d", response.StatusCode)
	}
	body, _ := io.ReadAll(response.Body)
	if !strings.Contains(string(body), "diverged") {
		t.Fatalf("divergence reason missing: %s", body)
	}
	emitted, _ := io.ReadAll(reader)
	line := string(emitted)
	if !strings.HasPrefix(line, DivergenceMarker) {
		t.Fatalf("structured divergence marker missing: %q", line)
	}
	var report map[string]any
	raw := strings.TrimSpace(strings.TrimPrefix(line, DivergenceMarker))
	if json.Unmarshal([]byte(raw), &report) != nil {
		t.Fatalf("divergence report is not JSON: %q", raw)
	}
	if report["protocol"] != "http" {
		t.Fatalf("divergence report lost the protocol: %v", report)
	}
}

func TestTruncatedRecordedBodyFailsClosed(t *testing.T) {
	payload := `{
      "format": "reproit-backend-capture", "version": 2,
      "operation": "GET /blob", "oracle": "backend-server-error",
      "events": [{"kind": "effect", "sequence": 1, "effect": "call",
        "exchange": {"protocol": "http",
          "request": {"method": "GET", "url": "http://pricing.internal/blob"},
          "response": {"status": 200, "truncated": true, "bodyBytes": 9000,
                       "bodySha256": "deadbeef"}}}]}`
	loaded := loadedCapture(t, payload)
	request, err := http.NewRequest(http.MethodGet, "http://pricing.internal/blob", nil)
	if err != nil {
		t.Fatal(err)
	}
	stderr := os.Stderr
	devNull, _ := os.Open(os.DevNull)
	os.Stderr = devNull
	response, err := loaded.serveHTTP(request)
	os.Stderr = stderr
	_ = devNull.Close()
	if err != nil {
		t.Fatal(err)
	}
	if response.StatusCode != 599 {
		t.Fatalf("truncated body was served instead of failing closed: %d",
			response.StatusCode)
	}
	body, _ := io.ReadAll(response.Body)
	if !strings.Contains(string(body), "truncated-exchange-body") {
		t.Fatalf("truncation reason missing: %s", body)
	}
}

func TestStrictOrdinalMatchingRefusesToSkipAnExchange(t *testing.T) {
	payload := `{
      "format": "reproit-backend-capture", "version": 2,
      "operation": "GET /quote", "oracle": "backend-server-error",
      "events": [
        {"kind": "effect", "sequence": 1, "effect": "call",
         "exchange": {"protocol": "http",
           "request": {"method": "GET", "url": "http://svc/first"},
           "response": {"status": 200}}},
        {"kind": "effect", "sequence": 2, "effect": "call",
         "exchange": {"protocol": "http",
           "request": {"method": "GET", "url": "http://svc/second"},
           "response": {"status": 200}}}]}`
	loaded := loadedCapture(t, payload)
	stderr := os.Stderr
	devNull, _ := os.Open(os.DevNull)
	os.Stderr = devNull
	// Asking for the SECOND exchange first must diverge, not silently skip.
	request, _ := http.NewRequest(http.MethodGet, "http://svc/second", nil)
	response, err := loaded.serveHTTP(request)
	os.Stderr = stderr
	_ = devNull.Close()
	if err != nil {
		t.Fatal(err)
	}
	if response.StatusCode != 599 {
		t.Fatalf("out-of-order call was matched fuzzily: %d", response.StatusCode)
	}
}

func TestRedactionPlaceholdersMatchAnyLiveValue(t *testing.T) {
	recorded := map[string]any{
		"password": map[string]any{"$reproit": map[string]any{"redacted": true}},
		"item":     "widget",
	}
	live := map[string]any{"password": "whatever-it-is-now", "item": "widget"}
	if !matchesRecorded(recorded, live) {
		t.Fatal("a redaction placeholder failed to match the live value")
	}
	if matchesRecorded(recorded, map[string]any{"password": "x", "item": "other"}) {
		t.Fatal("a non-secret mismatch was accepted")
	}
}

func TestReplayRNGIsDeterministicFromTheSeed(t *testing.T) {
	first := &ReplayRNG{state: 0x00ff00ff00ff00ff | 1}
	second := &ReplayRNG{state: 0x00ff00ff00ff00ff | 1}
	for index := 0; index < 4; index++ {
		left, right := first.Float64(), second.Float64()
		if left != right {
			t.Fatalf("seeded stream diverged at draw %d: %v vs %v", index, left, right)
		}
		if left < 0 || left >= 1 {
			t.Fatalf("draw outside [0, 1): %v", left)
		}
	}
}
