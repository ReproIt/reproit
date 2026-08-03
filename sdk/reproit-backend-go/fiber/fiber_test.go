// Functional test: a Fiber app with a planted 500, exercised through
// app.Test, with a real local stub ingest server for the capture batch.
package reproitfiber

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

	"github.com/gofiber/fiber/v2"
	reproit "github.com/ReproIt/reproit/sdk/reproit-backend-go"
)

func TestPlanted500ShipsATaggedFindingBatchAndScanHeaderRoundTrips(t *testing.T) {
	var mu sync.Mutex
	var auth []string
	var batches []map[string]any
	ingest := httptest.NewServer(http.HandlerFunc(
		func(w http.ResponseWriter, r *http.Request) {
			body, _ := io.ReadAll(r.Body)
			var batch map[string]any
			if err := json.Unmarshal(body, &batch); err != nil {
				http.Error(w, "bad batch", http.StatusBadRequest)
				return
			}
			mu.Lock()
			auth = append(auth, r.Header.Get("Authorization"))
			batches = append(batches, batch)
			mu.Unlock()
			_, _ = w.Write([]byte(`{"accepted":true}`))
		}))
	defer ingest.Close()

	config := reproit.NewCaptureConfig(ingest.URL+"/v1/events", "sk_live_test", "app-e2e")
	config.Build = "9.9.9"
	config.FlushInterval = 100 * time.Millisecond
	capture := reproit.NewCapture(config)
	if capture == nil {
		t.Fatal("capture must start")
	}

	app := fiber.New()
	app.Use(New(Options{Capture: capture}))
	app.Get("/ok", func(c *fiber.Ctx) error {
		return c.JSON(fiber.Map{"ok": true})
	})
	app.Post("/boom", func(c *fiber.Ctx) error {
		if trace := From(c); trace != nil {
			_ = trace.Effect(reproit.EffectWrite,
				reproit.EffectOptions{Resource: "orders", Key: "1"})
		}
		return c.Status(fiber.StatusInternalServerError).
			JSON(fiber.Map{"error": "boom"})
	})

	body := bytes.NewReader([]byte(`{"item":"widget","apiKey":"sk_live_leak"}`))
	request := httptest.NewRequest(http.MethodPost, "/boom", body)
	request.Header.Set("Content-Type", "application/json")
	response, err := app.Test(request)
	if err != nil {
		t.Fatal(err)
	}
	if response.StatusCode != http.StatusInternalServerError {
		t.Fatalf("planted bug returned %d", response.StatusCode)
	}
	if !capture.Flush(5 * time.Second) {
		t.Fatal("flush timed out")
	}

	mu.Lock()
	if len(batches) != 1 {
		mu.Unlock()
		t.Fatalf("expected 1 ingest batch, got %d", len(batches))
	}
	batch := batches[0]
	authorization := auth[0]
	mu.Unlock()
	if authorization != "Bearer sk_live_test" {
		t.Fatalf("wrong authorization: %q", authorization)
	}
	// capture-batch-v1: the universal causal contract, not the legacy
	// event batch. The observation carries the oracle identity and the
	// trigger carries the redacted input.
	if batch["projectId"] != "app-e2e" {
		t.Fatalf("wrong projectId: %v", batch["projectId"])
	}
	events := batch["events"].([]any)
	kinds := make([]string, 0, len(events))
	var observation, trigger map[string]any
	for _, item := range events {
		event := item.(map[string]any)["event"].(map[string]any)
		kind := event["kind"].(string)
		kinds = append(kinds, kind)
		switch kind {
		case "observation":
			observation = event
		case "trigger":
			trigger = event
		}
	}
	if len(kinds) < 2 || kinds[0] != "operation-start" || kinds[1] != "trigger" {
		t.Fatalf("capture sequence wrong: %v", kinds)
	}
	if observation == nil {
		t.Fatal("no failure observation in the batch")
	}
	signature := observation["failure"].(map[string]any)["signature"]
	if signature != reproit.ServerErrorOracle+":POST /boom" {
		t.Fatalf("observation not tagged %s: %v", reproit.ServerErrorOracle, signature)
	}
	if trigger == nil {
		t.Fatal("no trigger event in the batch")
	}
	inputBody := trigger["value"].(map[string]any)["value"].(map[string]any)["body"].(map[string]any)
	if inputBody["apiKey"].(map[string]any)["$reproit"] == nil {
		t.Fatal("apiKey shipped unredacted")
	}
	if inputBody["item"] != "widget" {
		t.Fatal("non-secret input field damaged")
	}

	// Scan-time round-trip: x-reproit-trace in, x-reproit-events out.
	scan := httptest.NewRequest(http.MethodGet, "/ok", nil)
	scan.Header.Set("x-reproit-trace", "trace-e2e")
	scan.Header.Set("x-reproit-actor", "alice")
	scanResponse, err := app.Test(scan)
	if err != nil {
		t.Fatal(err)
	}
	header := scanResponse.Header.Get("x-reproit-events")
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
	if traceEvents[0]["traceId"] != "trace-e2e" || traceEvents[0]["actor"] != "alice" {
		t.Fatalf("scan trace context wrong: %v", traceEvents[0])
	}
	last := traceEvents[len(traceEvents)-1]
	if last["kind"] != "return" || last["status"].(float64) != 200 {
		t.Fatalf("scan return event wrong: %v", last)
	}
	if stats := capture.Stats(); stats.CapturedOperations != 1 {
		t.Fatalf("captured %d operations, want 1", stats.CapturedOperations)
	}
}
