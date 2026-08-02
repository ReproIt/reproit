// Package reproitci is CI capture mode for reproit-backend-go: the flaky-CI
// wedge.
//
// `Wrap(t, suite)` binds one `testing.T` test to a trigger identity that is
// the TEST (suite + test name), not an inbound HTTP request. With
// `REPROIT_CI_CAPTURE=1` the test runs under its own capture-envelope trace:
// route outbound calls through the SDK boundaries (`reproit.WrapClient`,
// `RunDB`, `SQLDriver`) with the context `Wrap` returns via `Context()`, and
// every dependency exchange is recorded exactly as production capture does. A
// FAILING test spools a version-2 `reproit-backend-capture` capsule to a
// bounded on-disk spool. With `REPROIT_REPLAY` set the SAME wrapper skips
// every test but the capsule's named one while the SDK serves the recorded
// exchanges in process, and reports the observed result as a structured
// stderr marker for `reproit check`. Without either env the wrapper is inert.
//
// The wire is the existing capture payload: the test identity rides in the
// `operation` field as `test:<suite>#<test>`, and the failed assertion is the
// existing `backend-authored-invariant` registry oracle (a test IS an
// authored invariant). No new protocol fields, no new oracle ids.
//
// Go has no ambient async storage and `go test` merges the test binary's
// stderr into stdout, so two things are explicit here that Node hides:
// outbound calls must carry `Context()`, and a replay command must redirect
// stdout to stderr (`go test ... 1>&2`) so `reproit check` sees the markers.
//
// Honest limit: replay pins the envelope and the recorded exchanges, which is
// the whole boundary this SDK can see. A race the boundary cannot see
// (scheduling, shared memory) is not reproduced by this capsule; `reproit
// check` reports such runs Inconclusive, never a fake reproduction.
package reproitci

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"
	"unicode/utf8"

	reproit "github.com/reproit/reproit-backend"
)

const (
	// TestTriggerPrefix is the test-trigger identity prefix inside the
	// existing `operation` field.
	TestTriggerPrefix = "test:"
	// TestFailureOracle is the registry oracle a failed test capsule
	// carries: an authored invariant (the test's own assertion) was
	// violated. Existing id, not a new one.
	TestFailureOracle = "backend-authored-invariant"
	// ResultMarker and SpoolMarker are structured stderr markers `reproit
	// check` parses, like REPROIT:DIVERGENCE.
	ResultMarker = "REPROIT:CI-TEST "
	SpoolMarker  = "REPROIT:CI-CAPSULE "
	// Spool bounds. The cap covers the TOTAL bytes on disk; spilled capsules
	// beyond it are dropped and counted (in-process stats plus the on-disk
	// `dropped.count`), never silently.
	DefaultSpoolDir      = ".reproit/ci-spool"
	DefaultSpoolMaxBytes = 16 * 1024 * 1024
	spoolMaxFloorBytes   = 4 * 1024
	spoolMaxCeilBytes    = 64 * 1024 * 1024
	// Suite and test names share the operation field's 256-code-point bound.
	maxName       = 120
	maxErrorChars = 2048
)

// Stats is a point-in-time snapshot of the CI capture counters.
type Stats struct {
	SpooledCapsules uint64
	DroppedCapsules uint64
	FailedCaptures  uint64
}

var counters struct {
	spooled  atomic.Uint64
	dropped  atomic.Uint64
	failed   atomic.Uint64
	traceSeq atomic.Uint64
}

// CurrentStats returns a snapshot of the CI capture counters.
func CurrentStats() Stats {
	return Stats{
		SpooledCapsules: counters.spooled.Load(),
		DroppedCapsules: counters.dropped.Load(),
		FailedCaptures:  counters.failed.Load(),
	}
}

// T wraps one test. Failure calls made through it (Error/Errorf/Fatal/
// Fatalf) record the bounded failure message that becomes the capsule's
// recorded failure identity at capture time and the result marker's at
// replay; calls made on the underlying *testing.T still fail the test but
// leave the identity empty (which `reproit check` treats as "any failure
// reproduces").
type T struct {
	*testing.T
	ctx       context.Context
	operation string

	mu      sync.Mutex
	failure string
}

// Context returns the context outbound calls must carry so the SDK
// boundaries record onto (or replay against) this test's trace. Outside
// capture mode it is context.Background().
func (c *T) Context() context.Context { return c.ctx }

// Error records the failure identity, then fails via the underlying T.
func (c *T) Error(args ...any) {
	c.T.Helper()
	c.record(fmt.Sprint(args...))
	c.T.Error(args...)
}

// Errorf records the failure identity, then fails via the underlying T.
func (c *T) Errorf(format string, args ...any) {
	c.T.Helper()
	c.record(fmt.Sprintf(format, args...))
	c.T.Errorf(format, args...)
}

// Fatal records the failure identity, then aborts via the underlying T.
func (c *T) Fatal(args ...any) {
	c.T.Helper()
	c.record(fmt.Sprint(args...))
	c.T.Fatal(args...)
}

// Fatalf records the failure identity, then aborts via the underlying T.
func (c *T) Fatalf(format string, args ...any) {
	c.T.Helper()
	c.record(fmt.Sprintf(format, args...))
	c.T.Fatalf(format, args...)
}

// record keeps the FIRST bounded failure message: the identity `reproit
// check` compares between the recorded run and a replay.
func (c *T) record(message string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.failure == "" {
		c.failure = boundedError(message)
	}
}

func (c *T) recorded() string {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.failure
}

// Wrap binds one test to the CI capture/replay mode the environment selects.
// Call it first in the test body:
//
//	func TestOrderTotal(t *testing.T) {
//	    ct := reproitci.Wrap(t, "checkout")
//	    // outbound calls carry ct.Context(); assertions go through ct
//	}
func Wrap(t *testing.T, suite string) *T {
	t.Helper()
	operation := operationFor(suite, t.Name())
	if path := replayPath(); path != "" {
		return wrapReplay(t, operation, path)
	}
	if os.Getenv("REPROIT_CI_CAPTURE") == "1" {
		return wrapCapture(t, suite, operation)
	}
	return &T{T: t, ctx: context.Background()}
}

func wrapCapture(t *testing.T, suite, operation string) *T {
	t.Helper()
	trace, err := reproit.Begin(ciContext(), operation, reproit.BeginOptions{
		Input: map[string]any{
			"suite": boundedName(suite),
			"test":  boundedName(t.Name()),
		},
	})
	if err != nil {
		// Bounded names cannot produce this; a broken trace layer must not
		// silently run the suite uncaptured.
		t.Fatalf("reproit ci capture: %v", err)
	}
	wrapped := &T{
		T:         t,
		ctx:       reproit.ContextWithTrace(context.Background(), trace),
		operation: operation,
	}
	t.Cleanup(func() {
		if t.Failed() {
			finishAndSpool(trace, operation, wrapped.recorded())
			return
		}
		// An over-long passing trace has nothing to spool anyway.
		_ = trace.Finish(nil, 0, true, false)
	})
	return wrapped
}

func wrapReplay(t *testing.T, operation, path string) *T {
	t.Helper()
	// Pin the envelope (TZ, clock offset, seed) before test code runs.
	reproit.Init()
	target, err := replayTarget(path)
	if err != nil {
		t.Fatalf("reproit ci replay: %v", err)
	}
	if operation != target {
		// The capsule names exactly one test; everything else is skipped so
		// the run speaks for the named test alone.
		t.Skipf("reproit replay targets %s", target)
	}
	wrapped := &T{T: t, ctx: context.Background(), operation: operation}
	t.Cleanup(func() {
		status := "passed"
		failure := ""
		if t.Failed() {
			status = "failed"
			failure = wrapped.recorded()
		}
		reportResult(operation, status, failure)
	})
	return wrapped
}

// resultDetail serializes in node's field order: operation, status, failure.
type resultDetail struct {
	Operation string `json:"operation"`
	Status    string `json:"status"`
	Failure   string `json:"failure,omitempty"`
}

func reportResult(operation, status, failure string) {
	encoded, err := json.Marshal(resultDetail{
		Operation: operation, Status: status, Failure: failure,
	})
	if err != nil {
		counters.failed.Add(1)
		return
	}
	_, _ = os.Stderr.WriteString(ResultMarker + string(encoded) + "\n")
}

// finishAndSpool finishes the failed trace and writes the capsule. Capture
// must never mask the test's own failure: every error path only counts.
func finishAndSpool(trace *reproit.BackendTrace, operation, failure string) {
	defer func() {
		if recover() != nil {
			counters.failed.Add(1)
		}
	}()
	output := map[string]any{}
	if failure != "" {
		output["error"] = failure
	}
	if err := trace.Finish(output, 0, false, false); err != nil {
		counters.failed.Add(1)
		return
	}
	events := trace.Events()
	values := make([]any, 0, len(events))
	for _, event := range events {
		values = append(values, event)
	}
	payload := map[string]any{
		"format":    reproit.CaptureFormat,
		"version":   2,
		"operation": operation,
		"oracle":    TestFailureOracle,
		// Same envelope shape production capture records; the seed pins the
		// REPLAY run's randomness, it does not reproduce the test run's.
		"envelope": reproit.DeterminismEnvelope(events[0]["at"]),
		"events":   values,
	}
	spool(reproit.CanonicalJSON(payload), operation)
}

// spool writes one capsule inside the byte cap; over-cap capsules are
// dropped and counted, never silently.
func spool(body []byte, operation string) {
	dir := spoolDir()
	if err := os.MkdirAll(dir, 0o755); err != nil {
		counters.failed.Add(1)
		return
	}
	used := int64(0)
	entries, err := os.ReadDir(dir)
	if err != nil {
		counters.failed.Add(1)
		return
	}
	for _, entry := range entries {
		if !strings.HasSuffix(entry.Name(), ".json") {
			continue
		}
		// A concurrently removed entry counts as zero.
		if info, err := entry.Info(); err == nil {
			used += info.Size()
		}
	}
	if used+int64(len(body)) > spoolMaxBytes() {
		counters.dropped.Add(1)
		recordDrop(dir)
		return
	}
	digest := sha256.Sum256(body)
	file := filepath.Join(dir, "capsule-"+hex.EncodeToString(digest[:6])+".json")
	if err := os.WriteFile(file, body, 0o600); err != nil {
		counters.failed.Add(1)
		return
	}
	counters.spooled.Add(1)
	marker, err := json.Marshal(struct {
		File      string `json:"file"`
		Operation string `json:"operation"`
	}{File: file, Operation: operation})
	if err != nil {
		return
	}
	_, _ = os.Stderr.WriteString(SpoolMarker + string(marker) + "\n")
}

// recordDrop bumps the on-disk drop counter so a full spool is visible even
// after the process exits.
func recordDrop(dir string) {
	counter := filepath.Join(dir, "dropped.count")
	dropped := 0
	// First drop: the counter does not exist yet.
	if raw, err := os.ReadFile(counter); err == nil {
		if parsed, err := strconv.Atoi(strings.TrimSpace(string(raw))); err == nil {
			dropped = parsed
		}
	}
	_ = os.WriteFile(counter, []byte(strconv.Itoa(dropped+1)+"\n"), 0o600)
}

// replayTarget reads the capsule's operation; anything but a test trigger is
// rejected so the wrong capsule cannot silently pass a suite.
func replayTarget(path string) (string, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	var payload struct {
		Operation string `json:"operation"`
	}
	if err := json.Unmarshal(raw, &payload); err != nil {
		return "", err
	}
	if !strings.HasPrefix(payload.Operation, TestTriggerPrefix) {
		return "", fmt.Errorf(
			"REPROIT_REPLAY capsule does not carry a test trigger identity")
	}
	return payload.Operation, nil
}

func replayPath() string {
	return strings.TrimSpace(os.Getenv("REPROIT_REPLAY"))
}

func operationFor(suite, test string) string {
	return TestTriggerPrefix + boundedName(suite) + "#" + boundedName(test)
}

func boundedName(value string) string {
	value = strings.TrimSpace(value)
	if utf8.RuneCountInString(value) <= maxName {
		return value
	}
	return string([]rune(value)[:maxName])
}

func boundedError(message string) string {
	if utf8.RuneCountInString(message) <= maxErrorChars {
		return message
	}
	return string([]rune(message)[:maxErrorChars])
}

// ciContext synthesizes the trace context: the CI job stands where
// production stood.
func ciContext() *reproit.TraceContext {
	sequence := counters.traceSeq.Add(1)
	return &reproit.TraceContext{
		TraceID: "ci-" + strconv.FormatInt(time.Now().UnixMilli(), 10) +
			"-" + strconv.FormatUint(sequence, 10),
		Build:           ciCommit(),
		CaptureEnvelope: true,
	}
}

// ciCommit reads code identity from the common CI environment; never shells
// out to git.
func ciCommit() string {
	for _, name := range []string{"REPROIT_COMMIT", "GITHUB_SHA"} {
		if value := os.Getenv(name); tokenValid(value) {
			return value
		}
	}
	return ""
}

// tokenValid checks the ingest protocol token charset.
func tokenValid(value string) bool {
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

func spoolDir() string {
	if dir := os.Getenv("REPROIT_CI_SPOOL"); dir != "" {
		return dir
	}
	return DefaultSpoolDir
}

func spoolMaxBytes() int64 {
	parsed, err := strconv.ParseInt(os.Getenv("REPROIT_CI_SPOOL_MAX"), 10, 64)
	if err != nil {
		return DefaultSpoolMaxBytes
	}
	return min(int64(spoolMaxCeilBytes), max(int64(spoolMaxFloorBytes), parsed))
}
