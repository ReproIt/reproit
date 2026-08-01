// Outbound-exchange capture and hermetic replay for reproit-backend-go.
//
// Go port of the Node SDK's instrument.js + replay.js, following the Rust
// SDK's precedent: Go has no monkeypatching, so the boundary is explicit and
// OPT-IN. Route outbound HTTP through a Transport (or the client WrapClient
// returns) and database statements through RunDB, and every dependency
// exchange (request AND response) is recorded onto the ambient request trace,
// bounded and redacted at source.
//
// With REPROIT_REPLAY naming a `reproit-backend-capture` payload, the SAME
// entry points serve the recorded exchanges instead: strict per-protocol
// ordinal matching, `$reproit` redaction placeholders match any value, a
// truncated-at-capture body fails closed, and the first unmatched call emits
// a structured `REPROIT:DIVERGENCE` stderr line and answers 599 (HTTP) or an
// error (db). No live dependency is touched in replay mode.
//
// Capture failure is invisible to the host app: an instrumentation defect
// never breaks the caller's request.
package reproitbackend

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"hash"
	"io"
	"net/http"
	"net/url"
	"sort"
	"strconv"
	"strings"
	"sync/atomic"
)

const (
	// MaxExchangeBodyBytes is the inline body budget per exchange side.
	// Beyond it the body is dropped and only provable identity (byte count +
	// sha256) remains.
	MaxExchangeBodyBytes = 8 * 1024
	// maxExchangeHeaders caps recorded headers so events stay bounded.
	maxExchangeHeaders = 32
	// maxDBRows caps rows recorded per db result; beyond it the result is
	// marked truncated.
	maxDBRows = 64
	// MaxStreamChunks caps recorded stream chunk boundaries per exchange
	// (SSE / chunked responses, the LLM streaming shape). Beyond it the
	// boundaries are marked truncated and replay fails closed rather than
	// serve a wrong stream shape.
	MaxStreamChunks = 128
)

// exchangeCounters mirrors the Node reference's stats(): how many exchanges
// were recorded, how many bodies were reduced to identity, and how many
// capture attempts failed closed (including the per-trace event cap:
// MaxEvents overflow drops the exchange and counts here, never breaks the
// host request).
var exchangeCounters struct {
	captured  atomic.Uint64
	truncated atomic.Uint64
	failed    atomic.Uint64
}

// ExchangeStats is a point-in-time snapshot of the instrument counters.
type ExchangeStats struct {
	CapturedExchanges uint64
	TruncatedBodies   uint64
	FailedCaptures    uint64
}

// InstrumentStats returns a snapshot of the outbound-exchange counters.
func InstrumentStats() ExchangeStats {
	return ExchangeStats{
		CapturedExchanges: exchangeCounters.captured.Load(),
		TruncatedBodies:   exchangeCounters.truncated.Load(),
		FailedCaptures:    exchangeCounters.failed.Load(),
	}
}

// ContextWithTrace makes trace the ambient trace for Transport and RunDB
// calls made with the returned context. The net/http middleware does this
// automatically; call it directly only for hand-rolled servers and fixtures.
func ContextWithTrace(ctx context.Context, trace *BackendTrace) context.Context {
	return context.WithValue(ctx, traceContextKey{}, trace)
}

// Transport records every round trip as an exchange on the ambient trace, or
// serves the recorded exchange when REPROIT_REPLAY is set. Base defaults to
// http.DefaultTransport.
//
// Nothing is automatic: a client that does not use this Transport is
// invisible to capture and unavailable at replay.
type Transport struct {
	Base http.RoundTripper
}

// WrapClient returns a copy of client whose Transport records exchanges.
// A nil client wraps http.DefaultClient.
func WrapClient(client *http.Client) *http.Client {
	base := http.DefaultClient
	if client != nil {
		base = client
	}
	wrapped := *base
	wrapped.Transport = &Transport{Base: base.Transport}
	return &wrapped
}

func (t *Transport) base() http.RoundTripper {
	if t.Base != nil {
		return t.Base
	}
	return http.DefaultTransport
}

// RoundTrip implements http.RoundTripper.
func (t *Transport) RoundTrip(request *http.Request) (*http.Response, error) {
	if replay := session(); replay != nil {
		return replay.serveHTTP(request)
	}
	trace := FromContext(request.Context())
	if trace == nil {
		return t.base().RoundTrip(request)
	}
	requestBody := newBodyCollector()
	outbound := request
	if request.Body != nil && request.Body != http.NoBody {
		outbound = request.Clone(request.Context())
		outbound.Body = &teeReadCloser{source: request.Body, sink: requestBody}
	}
	response, err := t.base().RoundTrip(outbound)
	if err != nil || response == nil {
		return response, err
	}
	responseBody := newBodyCollector()
	recorded := false
	record := func() {
		if recorded {
			return
		}
		recorded = true
		recordHTTPExchange(trace, request, requestBody, response, responseBody)
	}
	response.Body = &teeReadCloser{
		source: response.Body,
		sink:   responseBody,
		onDone: record,
	}
	return response, nil
}

// recordHTTPExchange writes one exchange effect. It never returns an error:
// a finished or overflowed trace must not break the host's request.
func recordHTTPExchange(
	trace *BackendTrace,
	request *http.Request,
	requestBody *bodyCollector,
	response *http.Response,
	responseBody *bodyCollector,
) {
	defer func() {
		if recover() != nil {
			exchangeCounters.failed.Add(1)
		}
	}()
	requestValue := map[string]any{
		"method": request.Method,
		"url":    requestURL(request),
	}
	for key, value := range boundedHeaders(request.Header) {
		requestValue[key] = value
	}
	for key, value := range requestBody.result(request.Header.Get("Content-Type")) {
		requestValue[key] = value
	}
	responseValue := map[string]any{
		"status": json.Number(strconv.Itoa(response.StatusCode)),
	}
	for key, value := range boundedHeaders(response.Header) {
		responseValue[key] = value
	}
	contentType := response.Header.Get("Content-Type")
	for key, value := range responseBody.result(contentType) {
		responseValue[key] = value
	}
	// Stream shape (SSE / chunked): observed chunk boundaries, so the whole
	// stream is ONE logical exchange and replay can re-serve it chunk for
	// chunk. A truncated inline body already fails closed, so boundaries are
	// only kept for bodies recorded verbatim.
	if responseValue["truncated"] != true {
		stream := responseBody.stream(strings.Contains(contentType, "text/event-stream"))
		if stream != nil {
			responseValue["stream"] = stream
		}
	}
	err := trace.Exchange(EffectCall, ExchangeOptions{
		Resource: request.URL.Host,
		Key:      request.Method + " " + urlPathAndQuery(requestURL(request)),
		Exchange: map[string]any{
			"protocol": "http",
			"request":  requestValue,
			"response": responseValue,
		},
	})
	if err != nil {
		// The trace finished or hit its event cap; the exchange is dropped
		// and counted, the host request goes on.
		exchangeCounters.failed.Add(1)
		return
	}
	exchangeCounters.captured.Add(1)
}

func requestURL(request *http.Request) string {
	if request.URL == nil {
		return ""
	}
	return request.URL.String()
}

// bodyCollector hashes every byte while retaining at most the inline budget,
// so a truncated body still carries provable identity without unbounded
// memory. It also records the observed chunk boundaries (one push per Read
// the app performed), bounded by MaxStreamChunks; boundaries past the cap
// are counted, never guessed.
type bodyCollector struct {
	digest            hash.Hash
	held              bytes.Buffer
	total             int
	boundaries        []int
	droppedBoundaries int
}

func newBodyCollector() *bodyCollector {
	return &bodyCollector{digest: sha256.New()}
}

func (c *bodyCollector) push(chunk []byte) {
	if len(chunk) == 0 {
		return
	}
	c.total += len(chunk)
	c.digest.Write(chunk)
	if len(c.boundaries) < MaxStreamChunks {
		c.boundaries = append(c.boundaries, len(chunk))
	} else {
		c.droppedBoundaries++
	}
	if remaining := MaxExchangeBodyBytes - c.held.Len(); remaining > 0 {
		if len(chunk) > remaining {
			c.held.Write(chunk[:remaining])
		} else {
			c.held.Write(chunk)
		}
	}
}

// result renders the recorded body fields: absent when empty, verbatim (JSON
// parsed when declared) within budget, identity only beyond it.
func (c *bodyCollector) result(contentType string) map[string]any {
	if c == nil || c.total == 0 {
		return nil
	}
	if c.total > MaxExchangeBodyBytes {
		exchangeCounters.truncated.Add(1)
		return map[string]any{
			"bodyBytes":  json.Number(strconv.Itoa(c.total)),
			"bodySha256": hex.EncodeToString(c.digest.Sum(nil)),
			"truncated":  true,
		}
	}
	return boundedBody(c.held.Bytes(), contentType)
}

// stream renders the observed chunk boundaries as byte lengths. Recorded
// when the response is a stream (SSE always; anything else only when it
// actually arrived in more than one chunk, since a single-chunk body replays
// identically without them). Boundaries past the cap are counted, never
// guessed, and replay fails closed on the truncation marker.
func (c *bodyCollector) stream(isEventStream bool) map[string]any {
	if c == nil || len(c.boundaries) == 0 {
		return nil
	}
	if !isEventStream && len(c.boundaries) < 2 && c.droppedBoundaries == 0 {
		return nil
	}
	chunks := make([]any, 0, len(c.boundaries))
	for _, length := range c.boundaries {
		chunks = append(chunks, json.Number(strconv.Itoa(length)))
	}
	if c.droppedBoundaries > 0 {
		return map[string]any{"chunks": chunks, "truncated": true}
	}
	return map[string]any{"chunks": chunks}
}

// boundedBody renders body fields for an already-buffered payload.
func boundedBody(body []byte, contentType string) map[string]any {
	if len(body) == 0 {
		return nil
	}
	if len(body) > MaxExchangeBodyBytes {
		exchangeCounters.truncated.Add(1)
		digest := sha256.Sum256(body)
		return map[string]any{
			"bodyBytes":  json.Number(strconv.Itoa(len(body))),
			"bodySha256": hex.EncodeToString(digest[:]),
			"truncated":  true,
		}
	}
	if strings.Contains(contentType, "application/json") {
		decoder := json.NewDecoder(bytes.NewReader(body))
		decoder.UseNumber()
		var decoded any
		if decoder.Decode(&decoded) == nil {
			return map[string]any{"body": decoded}
		}
		// Declared JSON that does not parse is recorded as text below.
	}
	return map[string]any{"body": string(body)}
}

func boundedHeaders(headers http.Header) map[string]any {
	if len(headers) == 0 {
		return nil
	}
	// http.Header iteration order is random; sort so the recorded subset is
	// stable when the cap truncates. The order is over the LOWERCASED name,
	// which is the name that gets recorded: sorting the wire spelling puts
	// `X-Trace` before `content-type` and picks a different subset.
	names := make([]string, 0, len(headers))
	for name := range headers {
		names = append(names, name)
	}
	sort.SliceStable(names, func(i, j int) bool {
		return strings.ToLower(names[i]) < strings.ToLower(names[j])
	})
	fields := make(map[string]any, len(names))
	for _, name := range names {
		if len(fields) >= maxExchangeHeaders {
			break
		}
		fields[strings.ToLower(name)] = strings.Join(headers[name], ", ")
	}
	if len(fields) == 0 {
		return nil
	}
	return map[string]any{"headers": fields}
}

// teeReadCloser copies everything read into the collector and reports EOF or
// Close exactly once, so an exchange is recorded when the body completes.
type teeReadCloser struct {
	source io.ReadCloser
	sink   *bodyCollector
	onDone func()
	done   bool
}

func (t *teeReadCloser) Read(buffer []byte) (int, error) {
	read, err := t.source.Read(buffer)
	if read > 0 {
		t.sink.push(buffer[:read])
	}
	if err != nil {
		t.finish()
	}
	return read, err
}

func (t *teeReadCloser) Close() error {
	t.finish()
	return t.source.Close()
}

func (t *teeReadCloser) finish() {
	if t.done {
		return
	}
	t.done = true
	if t.onDone != nil {
		t.onDone()
	}
}

// RecordExchange records one exchange on the ambient trace directly. It is
// the escape hatch for protocols this SDK has no boundary for; request and
// response are recorded as given (bounded and redacted by the trace layer).
func RecordExchange(ctx context.Context, protocol string, request, response any) {
	defer func() { _ = recover() }()
	trace := FromContext(ctx)
	if trace == nil || protocol == "" {
		return
	}
	_ = trace.Exchange(EffectCall, ExchangeOptions{
		Resource: protocol,
		Exchange: map[string]any{
			"protocol": protocol,
			"request":  request,
			"response": response,
		},
	})
}

// DBOutcome is the recorded result of one statement.
type DBOutcome struct {
	Command  string
	RowCount uint64
	Rows     []any
}

// DBError is a recorded statement failure. It implements error so RunDB can
// return recorded failures unchanged at replay.
type DBError struct {
	Message string
	Code    string
}

func (e *DBError) Error() string { return e.Message }

// RunDB routes one database statement through the exchange boundary.
//
// Capture mode runs live and records the statement with its outcome; replay
// mode serves the recorded outcome and never calls live, so no database is
// touched. Go has no driver to monkeypatch, so anything not routed through
// RunDB is invisible to capture and unavailable at replay.
func RunDB(
	ctx context.Context,
	text string,
	values []any,
	live func() (DBOutcome, error),
) (DBOutcome, error) {
	if replay := session(); replay != nil {
		return replay.serveDB(text, values)
	}
	outcome, err := live()
	trace := FromContext(ctx)
	if trace == nil {
		return outcome, err
	}
	func() {
		defer func() {
			if recover() != nil {
				exchangeCounters.failed.Add(1)
			}
		}()
		request := map[string]any{"text": text}
		if len(values) > 0 {
			request["values"] = values
		}
		recordErr := trace.Exchange(dbEffectKind(text), ExchangeOptions{
			Resource: "pg",
			Key:      truncate(text, 256),
			Exchange: map[string]any{
				"protocol": "pg",
				"request":  request,
				"response": dbOutcomeValue(outcome, err),
			},
		})
		if recordErr != nil {
			exchangeCounters.failed.Add(1)
			return
		}
		exchangeCounters.captured.Add(1)
	}()
	return outcome, err
}

// dbEffectKind keeps reads as reads so state oracles retain their meaning;
// everything else is a write.
func dbEffectKind(text string) EffectKind {
	verb := strings.ToUpper(truncate(strings.TrimLeft(text, " \t\r\n"), 8))
	if strings.HasPrefix(verb, "SELECT") || strings.HasPrefix(verb, "SHOW") {
		return EffectRead
	}
	return EffectWrite
}

func dbOutcomeValue(outcome DBOutcome, err error) map[string]any {
	if err != nil {
		code := ""
		var typed *DBError
		if errors.As(err, &typed) {
			code = typed.Code
		}
		failure := map[string]any{"message": err.Error()}
		if code != "" {
			failure["code"] = code
		} else {
			failure["code"] = nil
		}
		return map[string]any{"error": failure}
	}
	rows := outcome.Rows
	truncated := len(rows) > maxDBRows
	if truncated {
		rows = rows[:maxDBRows]
	}
	items := make([]any, 0, len(rows))
	items = append(items, rows...)
	value := map[string]any{
		"command":  commandValue(outcome.Command),
		"rowCount": json.Number(strconv.FormatUint(outcome.RowCount, 10)),
		"rows":     items,
	}
	if truncated {
		value["truncated"] = true
	}
	return value
}

func commandValue(command string) any {
	if command == "" {
		return nil
	}
	return command
}

// urlPathAndQuery reduces a URL to the part matching compares, so a replay
// against a different host or port still matches the recorded call.
func urlPathAndQuery(raw string) string {
	parsed, err := url.Parse(raw)
	if err != nil || parsed.Path == "" && parsed.RawQuery == "" {
		return raw
	}
	if parsed.RawQuery != "" {
		return parsed.Path + "?" + parsed.RawQuery
	}
	return parsed.Path
}
