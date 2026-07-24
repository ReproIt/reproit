package reproitbackend

import (
	"encoding/json"
	"strings"
	"sync"
	"testing"
)

func finishedTrace(t *testing.T, status int, success bool) *BackendTrace {
	t.Helper()
	context := &TraceContext{TraceID: "cap-1-1", Build: "1.2.3"}
	trace, err := Begin(context, "createOrder", BeginOptions{
		Input: HTTPInput{
			Body: map[string]any{"item": "widget", "qty": 2},
		}.Value(),
	})
	if err != nil {
		t.Fatal(err)
	}
	err = trace.Effect(EffectRead, EffectOptions{Resource: "inventory", Key: "widget"})
	if err != nil {
		t.Fatal(err)
	}
	if err := trace.Finish(map[string]any{"error": "boom"}, status, success, true); err != nil {
		t.Fatal(err)
	}
	return trace
}

func batchFor(t *testing.T, status int, success bool) map[string]any {
	t.Helper()
	capture := &Capture{config: CaptureConfig{
		Endpoint: "http://c/v1/events", APIKey: "sk", AppID: "app-demo", Build: "1.2.3",
	}}
	trace := finishedTrace(t, status, success)
	return capture.buildBatch([]capturedOperation{{
		operation: "createOrder", status: status, events: trace.Events(),
	}})
}

func TestServerErrorBatchIsATaggedEventBatch(t *testing.T) {
	batch := batchFor(t, 500, false)
	if batch["version"] != 1 || batch["appId"] != "app-demo" {
		t.Fatalf("batch envelope wrong: %v", batch)
	}
	deployment := batch["deployment"].(map[string]any)
	if deployment["version"] != "1.2.3" {
		t.Fatal("deployment version lost")
	}
	frames := batch["frames"].([]any)
	if len(frames) != 4 {
		t.Fatalf("expected 3 backend frames + 1 finding, got %d", len(frames))
	}
	finding := frames[3].(map[string]any)["event"].(map[string]any)
	if finding["kind"] != "finding" {
		t.Fatal("finding frame missing")
	}
	identity := finding["identity"].(map[string]any)
	if identity["oracle"] != ServerErrorOracle {
		t.Fatalf("finding not tagged %s: %v", ServerErrorOracle, identity)
	}
	context := finding["context"].(map[string]any)
	if context["capture"] != "reproit-backend-go" {
		t.Fatal("capture origin missing")
	}
	payload := context["reproitCapture"].(map[string]any)
	if payload["format"] != CaptureFormat || payload["operation"] != "createOrder" {
		t.Fatalf("capture payload wrong: %v", payload)
	}
	events := payload["events"].([]any)
	if len(events) != 3 {
		t.Fatalf("expected full start/effect/return sequence, got %d", len(events))
	}
	// Redaction happened before anything left the process boundary.
	start := events[0].(map[string]any)
	body := start["input"].(map[string]any)["body"].(map[string]any)
	if body["item"] != "widget" {
		t.Fatal("capture events lost the redacted start input")
	}
}

func TestHealthyOperationsShipBackendFramesWithoutAFinding(t *testing.T) {
	batch := batchFor(t, 201, true)
	frames := batch["frames"].([]any)
	if len(frames) != 3 {
		t.Fatalf("expected 3 backend frames, got %d", len(frames))
	}
	for _, item := range frames {
		event := item.(map[string]any)["event"].(map[string]any)
		if event["kind"] != "backend" {
			t.Fatalf("unexpected frame kind: %v", event["kind"])
		}
	}
}

func TestOversizedCapturesDropTrailingEffectsFirst(t *testing.T) {
	events := finishedTrace(t, 500, false).Events()
	filler := strings.Repeat("x", maxCaptureJSONBytes)
	oversized := append(events[:2:2], map[string]any{
		"kind": "effect", "effect": "write", "resource": filler,
	}, events[2])
	payload, dropped, ok := capturePayload(capturedOperation{
		operation: "createOrder", status: 500, events: oversized,
	})
	if !ok || dropped != 1 {
		t.Fatalf("expected 1 dropped effect, got ok=%v dropped=%d", ok, dropped)
	}
	kept := payload["events"].([]any)
	if len(kept) != 3 {
		t.Fatalf("expected 3 kept events, got %d", len(kept))
	}
	second := kept[1].(map[string]any)
	if second["kind"] != "effect" || second["resource"] != "inventory" {
		t.Fatalf("wrong effect kept: %v", second)
	}
}

func TestUnusableConfigsDisableCaptureInsteadOfFailing(t *testing.T) {
	if NewCapture(NewCaptureConfig("", "sk", "app")) != nil {
		t.Fatal("empty endpoint accepted")
	}
	if NewCapture(NewCaptureConfig("http://c", "", "app")) != nil {
		t.Fatal("empty key accepted")
	}
	if NewCapture(NewCaptureConfig("http://c", "sk", "bad app id")) != nil {
		t.Fatal("invalid app id accepted")
	}
	config := NewCaptureConfig("http://c", "sk", "app")
	config.Build = "bad build!"
	if NewCapture(config) != nil {
		t.Fatal("invalid build accepted")
	}
}

func TestQueueDropsOldestOnOverflow(t *testing.T) {
	// No worker goroutine: enqueue directly against the bound.
	capture := &Capture{config: NewCaptureConfig("http://c", "sk", "app")}
	capture.signal = sync.NewCond(&capture.mu)
	for index := 0; index < maxQueueOperations+5; index++ {
		trace := finishedTrace(t, 500, false)
		capture.Record(trace)
	}
	if len(capture.queue) != maxQueueOperations {
		t.Fatalf("queue depth %d, want %d", len(capture.queue), maxQueueOperations)
	}
	stats := capture.Stats()
	if stats.DroppedOperations != 5 {
		t.Fatalf("dropped %d, want 5", stats.DroppedOperations)
	}
	if stats.CapturedOperations != maxQueueOperations+5 {
		t.Fatalf("captured %d", stats.CapturedOperations)
	}
}

func TestHealthyOperationsAreNotCapturedByDefault(t *testing.T) {
	capture := &Capture{config: NewCaptureConfig("http://c", "sk", "app")}
	capture.signal = sync.NewCond(&capture.mu)
	capture.Record(finishedTrace(t, 200, true))
	if len(capture.queue) != 0 {
		t.Fatal("healthy operation captured with sampling disabled")
	}
	capture.Record(finishedTrace(t, 200, false))
	if len(capture.queue) != 1 {
		t.Fatal("success == false operation must always be captured")
	}
}

func TestConfigBoundsAreClamped(t *testing.T) {
	config := NewCaptureConfig("http://c", "sk", "app")
	config.FlushInterval = 0
	config.RetryLimit = 99
	capture := NewCapture(config)
	if capture == nil {
		t.Fatal("usable config rejected")
	}
	if capture.config.FlushInterval != minFlushInterval {
		t.Fatalf("flush interval floor not applied: %v", capture.config.FlushInterval)
	}
	if capture.config.RetryLimit != maxRetryLimit {
		t.Fatalf("retry cap not applied: %d", capture.config.RetryLimit)
	}
}

func TestBatchSequencesAreDenseAndTyped(t *testing.T) {
	batch := batchFor(t, 500, false)
	encoded := CanonicalJSON(batch)
	var decoded map[string]any
	if err := json.Unmarshal(encoded, &decoded); err != nil {
		t.Fatalf("batch is not valid JSON: %v", err)
	}
	frames := decoded["frames"].([]any)
	for index, item := range frames {
		frame := item.(map[string]any)
		if int(frame["sequence"].(float64)) != index+1 {
			t.Fatalf("frame %d has sequence %v", index, frame["sequence"])
		}
		if frame["runId"] != decoded["batchId"] {
			t.Fatal("frame runId does not match batchId")
		}
	}
}
