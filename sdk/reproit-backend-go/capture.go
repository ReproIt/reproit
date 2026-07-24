// Production capture mode: config-gated self-sampling upload of finished
// operation traces to the Reproit Cloud ingest endpoint (`/v1/events`).
//
// Go port of sdk/reproit-backend-rs/src/capture.rs. Scan-time tracing stays
// untouched: this file only adds a place to hand a finished BackendTrace when
// no `x-reproit-trace` header exists. Operations that end in a server error
// (HTTP 5xx) or report success == false are always captured; healthy
// operations only under an optional per-mille baseline sample (default 0).
//
// Everything is bounded and capture failure is invisible to the host app: a
// fixed-depth queue drops oldest on overflow, batches and retries are capped,
// uploads run on one background goroutine, and Record never blocks or panics.
package reproitbackend

import (
	"bytes"
	"encoding/json"
	"net/http"
	"strconv"
	"sync"
	"sync/atomic"
	"time"
)

const (
	// CaptureFormat identifies the replayable capture object attached to the
	// finding context (`context.reproitCapture`).
	CaptureFormat  = "reproit-backend-capture"
	CaptureVersion = 1
	// ServerErrorOracle is the first-class registry oracle id for an
	// operation that returned HTTP 5xx.
	ServerErrorOracle = "backend-server-error"
)

// Bounds. Queue overflow drops the OLDEST pending operation; an oversized
// capture payload drops trailing effect events before it drops itself.
const (
	maxQueueOperations  = 64
	maxBatchOperations  = 16
	maxCaptureJSONBytes = 48 * 1024
	minFlushInterval    = 100 * time.Millisecond
	maxRetryLimit       = 5
)

// CaptureConfig configures capture mode. Build with NewCaptureConfig so the
// defaults match the other backend SDKs.
type CaptureConfig struct {
	// Endpoint is the full ingest URL, e.g. `https://cloud.example.com/v1/events`.
	Endpoint string
	// APIKey is the project API key, sent as `Authorization: Bearer`.
	APIKey string
	// AppID is the Cloud project app id the batches are posted under.
	AppID string
	// Build is an optional build/version identity stamped on batches.
	Build string
	// HealthySamplePerMille is the per-mille of healthy (successful,
	// non-5xx) operations captured as baseline evidence. 0 disables healthy
	// sampling entirely.
	HealthySamplePerMille int
	// FlushInterval is the gather window before a pending batch is sent.
	FlushInterval time.Duration
	// RequestTimeout is the per-request upload timeout.
	RequestTimeout time.Duration
	// RetryLimit is the upload retries per batch after the first attempt
	// (5xx/network only). Capped at 5.
	RetryLimit int
}

// NewCaptureConfig returns a config with the family defaults: no healthy
// sampling, 3 s flush interval, 5 s request timeout, 2 retries.
func NewCaptureConfig(endpoint, apiKey, appID string) CaptureConfig {
	return CaptureConfig{
		Endpoint:       endpoint,
		APIKey:         apiKey,
		AppID:          appID,
		FlushInterval:  3 * time.Second,
		RequestTimeout: 5 * time.Second,
		RetryLimit:     2,
	}
}

// CaptureStats is a point-in-time snapshot of the capture counters.
type CaptureStats struct {
	CapturedOperations uint64
	DroppedOperations  uint64
	SentBatches        uint64
	FailedBatches      uint64
}

type capturedOperation struct {
	operation string
	status    int // 0 = unknown
	events    []map[string]any
}

// Capture is the handle to the capture worker. Safe for concurrent use; all
// users share one queue and one upload goroutine.
type Capture struct {
	config CaptureConfig

	mu       sync.Mutex
	signal   *sync.Cond
	queue    []capturedOperation
	sending  bool
	flushNow bool

	captured atomic.Uint64
	dropped  atomic.Uint64
	sent     atomic.Uint64
	failed   atomic.Uint64
	rng      atomic.Uint64
	traceSeq atomic.Uint64
	batchSeq atomic.Uint64
}

// NewCapture starts capture mode. Returns nil (capture disabled, host
// unaffected) when the config is unusable: empty endpoint/key or identifiers
// the ingest protocol would reject.
func NewCapture(config CaptureConfig) *Capture {
	if config.Endpoint == "" || config.APIKey == "" || !validToken(config.AppID) {
		return nil
	}
	if config.Build != "" && !validToken(config.Build) {
		return nil
	}
	if config.FlushInterval < minFlushInterval {
		config.FlushInterval = minFlushInterval
	}
	if config.RequestTimeout <= 0 {
		config.RequestTimeout = 5 * time.Second
	}
	if config.RetryLimit < 0 {
		config.RetryLimit = 0
	}
	if config.RetryLimit > maxRetryLimit {
		config.RetryLimit = maxRetryLimit
	}
	if config.HealthySamplePerMille < 0 {
		config.HealthySamplePerMille = 0
	}
	capture := &Capture{config: config}
	capture.signal = sync.NewCond(&capture.mu)
	capture.rng.Store(uint64(time.Now().UnixMilli()) | 1)
	capture.traceSeq.Store(1)
	capture.batchSeq.Store(1)
	go capture.runWorker()
	return capture
}

// Context synthesizes a trace context for capture-mode operations, replacing
// the scan-time `x-reproit-trace` header requirement.
func (c *Capture) Context() *TraceContext {
	sequence := c.traceSeq.Add(1) - 1
	return &TraceContext{
		TraceID: "cap-" + strconv.FormatInt(time.Now().UnixMilli(), 10) +
			"-" + strconv.FormatUint(sequence, 10),
		Build: c.config.Build,
	}
}

// Record hands a finished trace to the sampler. Unfinished traces are
// ignored. Never blocks and never fails visibly; overflow drops the oldest
// queued operation.
func (c *Capture) Record(trace *BackendTrace) {
	defer func() {
		// Capture must never surface errors into the host app.
		_ = recover()
	}()
	if c == nil || trace == nil {
		return
	}
	events := trace.Events()
	var returned map[string]any
	for index := len(events) - 1; index >= 0; index-- {
		if kind, _ := events[index]["kind"].(string); kind == "return" {
			returned = events[index]
			break
		}
	}
	if returned == nil {
		return
	}
	success := true
	if value, ok := returned["success"].(bool); ok {
		success = value
	}
	status := 0
	if number, ok := returned["status"].(json.Number); ok {
		if parsed, err := strconv.Atoi(number.String()); err == nil && parsed >= 0 {
			status = parsed
		}
	}
	if success && status < 500 && !c.sampleHealthy() {
		return
	}
	operation, _ := events[0]["operation"].(string)
	if operation == "" {
		return
	}
	c.captured.Add(1)
	c.mu.Lock()
	c.queue = append(c.queue, capturedOperation{operation, status, events})
	if len(c.queue) > maxQueueOperations {
		c.queue = c.queue[1:]
		c.dropped.Add(1)
	}
	c.mu.Unlock()
	c.signal.Broadcast()
}

// Flush blocks up to timeout until every queued operation has been sent (or
// dropped). Returns false on timeout. Intended for tests, examples, and
// graceful shutdown; request handling never needs it.
func (c *Capture) Flush(timeout time.Duration) bool {
	deadline := time.Now().Add(timeout)
	done := make(chan struct{})
	go func() {
		// Waker: sync.Cond has no timed wait, so poke the condition until
		// the deadline passes or the flush completes.
		ticker := time.NewTicker(10 * time.Millisecond)
		defer ticker.Stop()
		for {
			select {
			case <-done:
				return
			case <-ticker.C:
				c.signal.Broadcast()
			}
		}
	}()
	defer close(done)
	c.mu.Lock()
	defer c.mu.Unlock()
	c.flushNow = true
	c.signal.Broadcast()
	for len(c.queue) > 0 || c.sending {
		if time.Now().After(deadline) {
			return false
		}
		c.signal.Wait()
	}
	return true
}

// Stats returns a snapshot of the capture counters.
func (c *Capture) Stats() CaptureStats {
	return CaptureStats{
		CapturedOperations: c.captured.Load(),
		DroppedOperations:  c.dropped.Load(),
		SentBatches:        c.sent.Load(),
		FailedBatches:      c.failed.Load(),
	}
}

func (c *Capture) sampleHealthy() bool {
	perMille := c.config.HealthySamplePerMille
	if perMille <= 0 {
		return false
	}
	if perMille >= 1000 {
		return true
	}
	// xorshift64 over a shared atomic seed; cheap and dependency-free.
	x := c.rng.Add(0x9e3779b97f4a7c15)
	x ^= x << 13
	x ^= x >> 7
	x ^= x << 17
	return x%1000 < uint64(perMille)
}

func (c *Capture) runWorker() {
	client := &http.Client{Timeout: c.config.RequestTimeout}
	for {
		operations := c.nextBatch()
		batch := c.buildBatch(operations)
		if c.send(client, batch) {
			c.sent.Add(1)
		} else {
			c.failed.Add(1)
			c.dropped.Add(uint64(len(operations)))
		}
		c.mu.Lock()
		c.sending = false
		c.mu.Unlock()
		c.signal.Broadcast()
	}
}

// nextBatch waits for work, gathers up to the batch cap within one flush
// interval, then drains. flushNow (set by Flush) cuts the gather short.
func (c *Capture) nextBatch() []capturedOperation {
	c.mu.Lock()
	defer c.mu.Unlock()
	for {
		if len(c.queue) > 0 {
			deadline := time.Now().Add(c.config.FlushInterval)
			for len(c.queue) < maxBatchOperations && !c.flushNow &&
				time.Now().Before(deadline) {
				c.timedWait(time.Until(deadline))
			}
			c.flushNow = false
			take := min(len(c.queue), maxBatchOperations)
			operations := append([]capturedOperation(nil), c.queue[:take]...)
			c.queue = append(c.queue[:0], c.queue[take:]...)
			c.sending = true
			return operations
		}
		c.flushNow = false
		c.signal.Wait()
	}
}

// timedWait releases the lock for at most the given duration. sync.Cond has
// no timed wait; a one-shot timer broadcast bounds the sleep.
func (c *Capture) timedWait(limit time.Duration) {
	if limit <= 0 {
		return
	}
	timer := time.AfterFunc(limit, c.signal.Broadcast)
	c.signal.Wait()
	timer.Stop()
}

// buildBatch builds one event-batch-v1 payload: every captured event ships
// as a `backend` frame, and each 5xx operation additionally ships a
// `finding` frame tagged `backend-server-error` whose context carries the
// full replayable capture object.
func (c *Capture) buildBatch(operations []capturedOperation) map[string]any {
	batchID := "cap-" + strconv.FormatInt(time.Now().UnixMilli(), 10) +
		"-" + strconv.FormatUint(c.batchSeq.Add(1)-1, 10)
	frames := []any{}
	sequence := 0
	frame := func(event map[string]any) {
		sequence++
		frames = append(frames, map[string]any{
			"runId":    batchID,
			"sequence": sequence,
			"scope":    map[string]any{"domain": "shared"},
			"event":    event,
		})
	}
	for _, operation := range operations {
		for _, event := range operation.events {
			frame(map[string]any{"kind": "backend", "evidence": event})
		}
		if operation.status < 500 {
			continue
		}
		signature := "backend:" + operation.operation
		message := "backend operation " + operation.operation +
			" returned HTTP " + strconv.Itoa(operation.status)
		context := map[string]any{"capture": "reproit-backend-go"}
		if c.config.Build != "" {
			context["build"] = map[string]any{"version": c.config.Build}
		}
		payload, droppedEffects, ok := capturePayload(operation)
		if !ok {
			context["captureOmitted"] = true
		} else {
			context["reproitCapture"] = payload
			if droppedEffects > 0 {
				context["captureDroppedEffects"] = droppedEffects
			}
		}
		frame(map[string]any{
			"kind":      "finding",
			"signature": signature,
			"message":   message,
			"identity": map[string]any{
				"oracle":    ServerErrorOracle,
				"invariant": "backend:server-error",
				"kind":      "server-error",
				"message":   message,
				"frame":     "",
				"trigger":   signature,
				"boundary":  signature,
			},
			"path":    []any{},
			"context": context,
		})
	}
	batch := map[string]any{
		"version":  1,
		"batchId":  batchID,
		"appId":    c.config.AppID,
		"frames":   frames,
		"evidence": []any{},
	}
	if c.config.Build != "" {
		batch["deployment"] = map[string]any{"version": c.config.Build}
	}
	return batch
}

func (c *Capture) send(client *http.Client, batch map[string]any) bool {
	body := CanonicalJSON(batch)
	for attempt := 0; attempt <= c.config.RetryLimit; attempt++ {
		request, err := http.NewRequest(http.MethodPost, c.config.Endpoint,
			bytes.NewReader(body))
		if err != nil {
			return false
		}
		request.Header.Set("Authorization", "Bearer "+c.config.APIKey)
		request.Header.Set("Content-Type", "application/json")
		response, err := client.Do(request)
		if err == nil {
			status := response.StatusCode
			response.Body.Close()
			if status >= 200 && status < 300 {
				return true
			}
			// A definitive client-side rejection cannot improve on retry.
			if status >= 400 && status < 500 {
				return false
			}
		}
		if attempt < c.config.RetryLimit {
			time.Sleep(time.Duration(200*attempt+200) * time.Millisecond)
		}
	}
	return false
}

// capturePayload builds the replayable capture object (`reproit debug
// replay-capture` input). Trailing effect events are dropped first when the
// payload exceeds the context budget; a payload that stays oversized with
// only start/return left is omitted entirely (ok == false).
func capturePayload(operation capturedOperation) (map[string]any, int, bool) {
	events := append([]map[string]any(nil), operation.events...)
	dropped := 0
	for {
		values := make([]any, 0, len(events))
		for _, event := range events {
			values = append(values, event)
		}
		payload := map[string]any{
			"format":    CaptureFormat,
			"version":   CaptureVersion,
			"operation": operation.operation,
			"oracle":    ServerErrorOracle,
			"events":    values,
		}
		if len(CanonicalJSON(payload)) <= maxCaptureJSONBytes {
			return payload, dropped, true
		}
		lastEffect := -1
		for index := len(events) - 1; index >= 0; index-- {
			if kind, _ := events[index]["kind"].(string); kind == "effect" {
				lastEffect = index
				break
			}
		}
		if lastEffect < 0 {
			return nil, dropped, false
		}
		events = append(events[:lastEffect], events[lastEffect+1:]...)
		dropped++
	}
}

// validToken checks the ingest protocol token charset (`validate_token` in
// reproit-protocol).
func validToken(value string) bool {
	if value == "" || len(value) > 128 {
		return false
	}
	for _, ch := range []byte(value) {
		alnum := (ch >= 'a' && ch <= 'z') || (ch >= 'A' && ch <= 'Z') ||
			(ch >= '0' && ch <= '9')
		if !alnum && ch != '-' && ch != '_' && ch != '.' && ch != ':' {
			return false
		}
	}
	return true
}
