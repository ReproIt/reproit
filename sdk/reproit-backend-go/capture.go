// Production capture mode: config-gated self-sampling upload of finished
// operation traces to the Reproit Cloud ingest endpoint
// (`/v1/capture-batches`).
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
	"fmt"
	"net/http"
	"os"
	"runtime"
	"strconv"
	"strings"
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
	// Endpoint is the full ingest URL, e.g.
	// `https://cloud.example.com/v1/capture-batches`.
	Endpoint string
	// APIKey is the project API key, sent as `Authorization: Bearer`.
	APIKey string
	// AppID is the Cloud project app id the batches are posted under.
	AppID string
	// Build is an optional build/version identity stamped on batches.
	Build string
	// Commit is the code identity for the capture. When unset, REPROIT_COMMIT
	// then GITHUB_SHA are consulted; never derived by shelling out to git.
	Commit string
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
	config.Commit = resolveCommit(config.Commit)
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
		// Capture-mode traces stamp per-event wall-clock and monotonic
		// offsets (the determinism envelope); scan-time traces never do.
		CaptureEnvelope: true,
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
			for len(c.queue) < 1 && !c.flushNow &&
				time.Now().Before(deadline) {
				c.timedWait(time.Until(deadline))
			}
			c.flushNow = false
			take := min(len(c.queue), 1)
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

// buildBatch builds one source-neutral capture-batch-v1 payload.
func (c *Capture) buildBatch(operations []capturedOperation) map[string]any {
	if len(operations) != 1 {
		panic("a causal capture batch must contain exactly one operation")
	}
	operation := operations[0]
	batchID := "cb-go-" + strconv.FormatInt(time.Now().UnixMilli(), 10) +
		"-" + strconv.FormatUint(c.batchSeq.Add(1)-1, 10)
	events := []any{}
	parent := ""
	traceID := ""
	first := map[string]any{}
	if len(operation.events) > 0 {
		first = operation.events[0]
		traceID, _ = first["traceId"].(string)
	}
	add := func(event map[string]any, mono any) {
		sequence := len(events) + 1
		eventID := "evt_backend-go_" + strconv.Itoa(sequence)
		parents := []any{}
		if parent != "" {
			parents = append(parents, parent)
		}
		// Real monotonic offsets from the trace's envelope stamps; the
		// ordinal fallback only applies to envelope-less traces.
		monotonic := any(sequence)
		if mono != nil {
			monotonic = mono
		}
		item := map[string]any{
			"id": eventID, "sequence": sequence, "monotonicNs": monotonic,
			"causalParentIds": parents, "event": event,
		}
		if traceID != "" {
			item["traceId"] = traceID
		}
		events = append(events, item)
		parent = eventID
	}
	monoOf := func(event map[string]any) any {
		if event == nil {
			return nil
		}
		return event["monoNs"]
	}
	firstMono := monoOf(first)
	add(map[string]any{"kind": "operation-start", "name": operation.operation}, firstMono)
	input, hasInput := first["input"]
	var inputValue map[string]any
	if hasInput && input != nil {
		inputValue = map[string]any{
			"representation": "replayable",
			"value":          input,
			"redaction":      "redacted-at-source",
		}
	} else {
		inputValue = map[string]any{
			"representation": "structural",
			"shape":          map[string]any{"type": "unknown"},
		}
	}
	add(map[string]any{
		"kind": "trigger", "trigger": "http-request",
		"subject": operation.operation, "value": inputValue,
	}, firstMono)
	// Determinism envelope: where and when the capture happened, and a seed
	// that makes REPLAY runs deterministic. Honesty note: the seed does not
	// reproduce the app's original randomness; it pins the replay's.
	add(map[string]any{
		"kind": "checkpoint", "name": "determinism-envelope",
		"attributes": c.determinismEnvelope(first["at"]),
	}, firstMono)
	for _, source := range operation.events {
		if source["kind"] != "effect" {
			continue
		}
		effect, _ := source["effect"].(string)
		if effect == "" {
			effect = "backend-effect"
		}
		subject, _ := source["resource"].(string)
		if subject == "" {
			subject = operation.operation
		}
		add(map[string]any{
			"kind": "effect", "effect": effect, "subject": subject,
			"value": map[string]any{
				"representation": "replayable",
				"value":          source,
				"redaction":      "redacted-at-source",
			},
		}, monoOf(source))
	}
	// Nest the raw return event exactly like the raw effect events, so the
	// batch can be projected back to a replayable backend capture. The
	// subject names the carrier: `backend_capture_from_batch` in
	// reproit-protocol keys the inversion on "operation-return".
	for _, source := range operation.events {
		if source["kind"] != "return" {
			continue
		}
		add(map[string]any{
			"kind": "effect", "effect": "operation-return",
			"subject": "operation-return",
			"value": map[string]any{
				"representation": "replayable",
				"value":          source,
				"redaction":      "redacted-at-source",
			},
		}, monoOf(source))
		break
	}
	outcome := "failed"
	for index := len(operation.events) - 1; index >= 0; index-- {
		if operation.events[index]["kind"] == "return" {
			if success, _ := operation.events[index]["success"].(bool); success {
				outcome = "succeeded"
			}
			break
		}
	}
	add(map[string]any{
		"kind": "operation-end", "name": operation.operation, "outcome": outcome,
	}, nil)
	if operation.status >= 500 {
		signature := ServerErrorOracle + ":" + operation.operation
		message := "backend operation " + operation.operation +
			" returned HTTP " + strconv.Itoa(operation.status)
		add(map[string]any{
			"kind": "observation",
			"failure": map[string]any{
				"observation":      "exception",
				"authority":        "runtime-diagnosis",
				"summary":          message,
				"signature":        signature,
				"observationPoint": operation.operation,
				"artifactIds":      []any{},
			},
		}, nil)
	}
	batch := map[string]any{
		"version": 1, "batchId": batchID, "projectId": c.config.AppID,
		"sessionId": func() string {
			if traceID != "" {
				return traceID
			}
			return batchID
		}(),
		"emitter": map[string]any{
			"id": "backend-go", "kind": "runtime-sdk",
			"component": "backend", "runtime": "go",
		},
		"observedAt": strconv.FormatInt(time.Now().UnixMilli(), 10),
		"policy": map[string]any{
			"consent": "application-telemetry", "retentionClass": "standard",
		},
		"capabilities": captureCapabilities(operation),
		"events":       events, "artifacts": []any{},
	}
	deployment := map[string]any{}
	if c.config.Build != "" {
		deployment["version"] = c.config.Build
	}
	if c.config.Commit != "" {
		deployment["commit"] = c.config.Commit
	}
	if len(deployment) > 0 {
		batch["deployment"] = deployment
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

// captureCapabilities declares what this capture proves. The network
// capability is claimed only when outbound exchanges were actually recorded,
// so a capsule never advertises replayability it does not have.
func captureCapabilities(operation capturedOperation) []any {
	hasExchanges := false
	for _, event := range operation.events {
		if exchange, ok := event["exchange"]; ok && exchange != nil {
			hasExchanges = true
			break
		}
	}
	list := []any{
		map[string]any{"capability": "http", "completeness": "complete"},
		map[string]any{
			"capability": "database", "completeness": "partial",
			"detail": "effect records do not prove complete database state capture",
		},
	}
	if hasExchanges {
		list = append(list, map[string]any{
			"capability": "network", "completeness": "complete",
			"detail": "outbound dependency exchanges recorded with responses",
		})
	}
	return list
}

// determinismEnvelope describes where and when the capture happened, plus the
// seed a replay pins its randomness to. The timezone comes from TZ when set;
// Go has no cheap IANA zone name for an unset TZ.
func (c *Capture) determinismEnvelope(observedAt any) map[string]any {
	seed := c.rng.Add(0x9e3779b97f4a7c15)
	return determinismEnvelopeFrom(seed, observedAt)
}

// DeterminismEnvelope builds a standalone determinism envelope for callers
// that write capture payloads themselves (fixtures, file sinks) instead of
// uploading through a Capture. Pass the first event's `at` stamp when one
// exists, nil otherwise.
func DeterminismEnvelope(observedAt any) map[string]any {
	return determinismEnvelopeFrom(uint64(time.Now().UnixNano())|1, observedAt)
}

func determinismEnvelopeFrom(seed uint64, observedAt any) map[string]any {
	seed ^= seed << 13
	seed ^= seed >> 7
	seed ^= seed << 17
	if seed == 0 {
		seed = 1
	}
	if observedAt == nil {
		observedAt = json.Number(strconv.FormatInt(time.Now().UnixMilli(), 10))
	}
	attributes := map[string]any{
		"observedAtMs": observedAt,
		"runtime":      "go",
		"os":           runtime.GOOS,
		"arch":         runtime.GOARCH,
		"replaySeed":   fmt.Sprintf("%016x", seed),
	}
	if tz := strings.TrimSpace(os.Getenv("TZ")); tz != "" {
		attributes["tz"] = tz
	}
	if digest := os.Getenv("REPROIT_IMAGE_DIGEST"); validToken(digest) {
		attributes["imageDigest"] = digest
	}
	return attributes
}

// resolveCommit reads code identity in priority order: explicit config, then
// the common CI and platform environment. Never shells out to git.
func resolveCommit(configured string) string {
	if validToken(configured) {
		return configured
	}
	for _, name := range []string{"REPROIT_COMMIT", "GITHUB_SHA"} {
		if value := os.Getenv(name); validToken(value) {
			return value
		}
	}
	return ""
}
