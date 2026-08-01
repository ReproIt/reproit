// Hermetic replay for reproit-backend-go.
//
// When REPROIT_REPLAY names a `reproit-backend-capture` payload, the same
// boundaries that record exchanges at capture time SERVE them instead, so the
// application re-executes against exactly what production saw with no live
// dependencies.
//
// Determinism is a contract here, not a similarity score. Matching is strict
// per-operation ordinals: within one operation (method plus path+query for
// HTTP, statement text for the database) exchanges are consumed in recorded
// order, so pooled database clients and LLM tool-call loops that interleave
// operations still match exactly. Recorded `$reproit` redaction placeholders
// match any value at their position; nothing else is tolerated. The first
// unmatched call is a DIVERGENCE: reported as a structured
// `REPROIT:DIVERGENCE` stderr line (with a `bodyDelta` naming WHERE the
// bodies differ; chat-shaped bodies name the first differing message index),
// then answered 599 (HTTP) or an error (db), never a fuzzy match. The marker
// line is BYTE-identical to the Node reference's (see ordered.go).
//
// The envelope pins the replay's determinism: TZ comes from the capture,
// ReplayNow returns the clock offset to the capture moment, and the seeded
// stream drives both math/rand's global source and ReplayRNG. Named gaps,
// documented rather than papered over: time.Now cannot be patched process
// wide (code reading it directly sees the real clock; use ReplayNow),
// math/rand/v2's global source cannot be reseeded, and crypto/rand is
// unpinnable by design. Honesty note: the seed makes REPLAY runs
// deterministic; it does not reproduce the randomness the app drew in
// production.
package reproitbackend

import (
	"bytes"
	"encoding/json"
	"io"
	mathrand "math/rand"
	"net/http"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"
)

// DivergenceMarker prefixes the structured divergence line, byte-identical
// to the Node reference's.
const DivergenceMarker = "REPROIT:DIVERGENCE "

type exchangeEntry struct {
	exchange omap
	consumed bool
}

type replaySession struct {
	envelope    omap
	clockOffset time.Duration
	mu          sync.Mutex
	exchanges   []*exchangeEntry
}

var (
	sessionOnce   sync.Once
	loadedSession *replaySession
)

// Init loads the replay session (when REPROIT_REPLAY is set) and pins the
// process envelope. Idempotent; the first Transport or RunDB call triggers it
// lazily, but calling it from main pins TZ before time-zone-sensitive code
// runs.
func Init() { _ = session() }

// Replaying reports whether this process is serving a recorded capture
// instead of touching live dependencies.
func Replaying() bool { return session() != nil }

// ReplayNow is the envelope-pinned clock: in replay mode it returns the
// current time offset to the capture moment; outside replay it is time.Now.
// Go cannot patch time.Now process wide (a named gap), so code that must see
// the recorded moment reads this instead.
func ReplayNow() time.Time {
	if replay := session(); replay != nil {
		return time.Now().Add(replay.clockOffset)
	}
	return time.Now()
}

func session() *replaySession {
	sessionOnce.Do(func() {
		path := strings.TrimSpace(os.Getenv("REPROIT_REPLAY"))
		if path == "" {
			return
		}
		loaded := loadReplaySession(path)
		if loaded == nil {
			return
		}
		loaded.pinEnvelope()
		loadedSession = loaded
	})
	return loadedSession
}

// loadReplaySession reads and validates the capture payload. A malformed or
// unsupported payload disables replay rather than half-serving it. The
// payload is decoded ORDER preserving (ordered.go) so recorded requests
// re-serialize byte-identically to the Node reference in divergence markers.
func loadReplaySession(path string) *replaySession {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	decoded, err := decodeOrderedJSON(raw)
	if err != nil {
		return nil
	}
	payload, ok := decoded.(omap)
	if !ok {
		return nil
	}
	if fieldString(payload, "format") != CaptureFormat {
		return nil
	}
	version, ok := numberValue(fieldOr(payload, "version"))
	if !ok || version < 1 || version > 2 {
		return nil
	}
	loaded := &replaySession{}
	if envelope, ok := fieldOr(payload, "envelope").(omap); ok {
		loaded.envelope = envelope
	}
	events, _ := fieldOr(payload, "events").([]any)
	for _, item := range events {
		event, ok := item.(omap)
		if !ok {
			continue
		}
		if fieldString(event, "kind") != "effect" {
			continue
		}
		exchange, ok := fieldOr(event, "exchange").(omap)
		if !ok {
			continue
		}
		loaded.exchanges = append(loaded.exchanges, &exchangeEntry{exchange: exchange})
	}
	return loaded
}

// pinEnvelope applies the capture's determinism envelope to this process:
// the timezone (Go caches time.Local, so the location is replaced directly
// as well as through TZ), the ReplayNow clock offset, and math/rand's global
// source seeded from replaySeed (best effort: math/rand/v2's global source
// has no seed hook and stays a named gap; ReplayRNG is the pinned stream).
func (s *replaySession) pinEnvelope() {
	if tz := fieldString(s.envelope, "tz"); strings.TrimSpace(tz) != "" {
		_ = os.Setenv("TZ", tz)
		if location, err := time.LoadLocation(tz); err == nil {
			time.Local = location
		}
	}
	if observedAtMs, ok := numberValue(fieldOr(s.envelope, "observedAtMs")); ok {
		observed := time.UnixMilli(int64(observedAtMs))
		s.clockOffset = time.Until(observed)
	}
	if seed, ok := replaySeedState(s.envelope); ok {
		// Deprecated since Go 1.20 but still the only global-source hook;
		// libraries drawing from math/rand's package functions see the
		// pinned stream after this.
		mathrand.Seed(int64(seed))
	}
}

// replaySeedState parses the envelope's replaySeed into the xorshift64*
// state shared by every SDK: first 16 hex digits, low bit forced on.
func replaySeedState(envelope omap) (uint64, bool) {
	seed := fieldString(envelope, "replaySeed")
	if seed == "" {
		return 0, false
	}
	if len(seed) > 16 {
		seed = seed[:16]
	}
	state, err := strconv.ParseUint(seed, 16, 64)
	if err != nil {
		return 0, false
	}
	return state | 1, true
}

// ReplayRNG is the deterministic xorshift64* stream seeded from the
// capture's replaySeed. It pins REPLAY determinism only; it does not
// reproduce the randomness the app drew in production.
type ReplayRNG struct {
	state uint64
}

// NewReplayRNG returns the seeded stream, or nil outside replay mode or when
// the capture carries no seed.
func NewReplayRNG() *ReplayRNG {
	replay := session()
	if replay == nil {
		return nil
	}
	state, ok := replaySeedState(replay.envelope)
	if !ok {
		return nil
	}
	return &ReplayRNG{state: state}
}

// Float64 returns the next draw in [0, 1), matching the Node and Rust
// SDKs' stream shape.
func (r *ReplayRNG) Float64() float64 {
	r.state ^= r.state << 13
	r.state ^= r.state >> 7
	r.state ^= r.state << 17
	mixed := r.state * 0x2545f4914f6cdd1d
	return float64(mixed>>11) / float64(uint64(1)<<53)
}

// operationKey is one operation's identity for ordinal matching: HTTP is
// method plus path+query, the database is the exact statement text.
func operationKey(protocol string, request any) string {
	if protocol == "http" {
		return fieldString(request, "method") + " " +
			urlPathAndQuery(fieldString(request, "url"))
	}
	return fieldString(request, "text")
}

// matched applies the strict per-operation ordinal rule: within one
// operation the next unconsumed exchange is the ONLY candidate, because
// skipping it silently would be a fuzzy match; other operations' exchanges
// may interleave (database pooling, tool-call loops). A nil result is a
// divergence, already reported.
func (s *replaySession) matched(protocol string, probe omap) omap {
	key := operationKey(protocol, probe)
	s.mu.Lock()
	var hit omap
	for _, entry := range s.exchanges {
		if entry.consumed || fieldString(entry.exchange, "protocol") != protocol {
			continue
		}
		request := fieldOr(entry.exchange, "request")
		if operationKey(protocol, request) != key {
			continue
		}
		ok := false
		if protocol == "http" {
			ok = httpRequestMatches(request, probe)
		} else {
			ok = dbRequestMatches(request, probe)
		}
		if ok {
			entry.consumed = true
			hit = entry.exchange
		}
		break
	}
	s.mu.Unlock()
	if hit != nil {
		return hit
	}
	s.diverge(protocol, probe)
	return nil
}

// diverge reports one unmatched probe as the structured marker line. Field
// insertion order and encoding mirror the Node reference exactly, so the
// line is byte-comparable across SDKs.
func (s *replaySession) diverge(protocol string, probe omap) {
	s.mu.Lock()
	key := operationKey(protocol, probe)
	consumed := 0
	var sameKey any
	var firstCandidate any
	for _, entry := range s.exchanges {
		if entry.consumed {
			consumed++
			continue
		}
		if fieldString(entry.exchange, "protocol") != protocol {
			continue
		}
		request := fieldOr(entry.exchange, "request")
		if firstCandidate == nil {
			firstCandidate = request
		}
		if sameKey == nil && operationKey(protocol, request) == key {
			sameKey = request
		}
	}
	total := len(s.exchanges)
	s.mu.Unlock()
	expected := sameKey
	if expected == nil {
		expected = firstCandidate
	}
	report := omap{
		{"protocol", protocol},
		{"got", probe},
		{"expected", expected},
		{"consumed", json.Number(strconv.Itoa(consumed))},
		{"total", json.Number(strconv.Itoa(total))},
	}
	// Prompt drift: when the recorded and live bodies both exist and differ,
	// name WHERE they differ. Chat-shaped bodies (OpenAI/Anthropic messages
	// arrays) name the first differing message index; unknown shapes fall
	// back to the byte offset of the first differing byte.
	if expected != nil {
		delta := bodyDelta(fieldOr(expected, "body"), fieldOr(probe, "body"))
		if delta != nil {
			report = append(report, omapEntry{"bodyDelta", delta})
		}
	}
	_, _ = os.Stderr.WriteString(DivergenceMarker + string(nodeJSON(report)) + "\n")
}

// chatMessages is the messages array of an OpenAI/Anthropic-shaped chat
// body, else nil.
func chatMessages(body any) []any {
	if messages, ok := fieldOr(body, "messages").([]any); ok {
		return messages
	}
	return nil
}

func deltaBytes(value any) []byte {
	if text, ok := value.(string); ok {
		return []byte(text)
	}
	return nodeJSON(value)
}

// bodyDelta locates the first difference between a recorded request body and
// a live one, modulo redaction placeholders. Nil when there is nothing to
// report (either body absent, or no difference the matcher would object to).
// The absent sentinel is deliberately distinct from an explicit null: a null
// body is a value the delta can be computed over, a missing one is not.
func bodyDelta(recorded, live any) any {
	if recorded == absent || live == absent {
		return nil
	}
	if matchesRecorded(recorded, live) {
		return nil
	}
	recordedMessages := chatMessages(recorded)
	liveMessages := chatMessages(live)
	if recordedMessages != nil && liveMessages != nil {
		bound := min(len(recordedMessages), len(liveMessages))
		index := -1
		for i := 0; i < bound; i++ {
			if !matchesRecorded(recordedMessages[i], liveMessages[i]) {
				index = i
				break
			}
		}
		// All shared indexes match: the drift is a longer/shorter
		// conversation, and the first differing message is the first
		// unshared one. If lengths also agree the drift is outside
		// `messages`; fall through to bytes.
		if index < 0 && len(recordedMessages) != len(liveMessages) {
			index = bound
		}
		if index >= 0 {
			return omap{
				{"kind", "message"},
				{"firstDifferingMessage", json.Number(strconv.Itoa(index))},
				{"recordedMessages", json.Number(strconv.Itoa(len(recordedMessages)))},
				{"liveMessages", json.Number(strconv.Itoa(len(liveMessages)))},
			}
		}
	}
	recordedBytes := deltaBytes(recorded)
	liveBytes := deltaBytes(live)
	bound := min(len(recordedBytes), len(liveBytes))
	offset := bound
	for i := 0; i < bound; i++ {
		if recordedBytes[i] != liveBytes[i] {
			offset = i
			break
		}
	}
	return omap{{"kind", "byte"}, {"offset", json.Number(strconv.Itoa(offset))}}
}

// serveHTTP answers one request entirely in process. A divergence and a
// truncated-at-capture body both serve a hard 599 so the application
// observes an attributable failure instead of a guess.
func (s *replaySession) serveHTTP(request *http.Request) (*http.Response, error) {
	body := []byte(nil)
	if request.Body != nil && request.Body != http.NoBody {
		body, _ = io.ReadAll(request.Body)
		_ = request.Body.Close()
	}
	probe := omap{
		{"method", request.Method},
		{"url", requestURL(request)},
	}
	if len(body) > 0 {
		probe = append(probe, omapEntry{
			"body", tryJSONOrdered(body, request.Header.Get("Content-Type")),
		})
	}
	recorded := s.matched("http", probe)
	if recorded == nil {
		return divergedResponse(request, "diverged"), nil
	}
	response, _ := fieldOr(recorded, "response").(omap)
	if truncated, _ := fieldOr(response, "truncated").(bool); truncated {
		// The capture kept identity but not bytes; serving a guessed body
		// would be a silent lie. Fail closed with the named reason.
		s.diverge("http", append(probe, omapEntry{"truncated", true}))
		return divergedResponse(request, "truncated-exchange-body"), nil
	}
	header := http.Header{}
	if headers, ok := fieldOr(response, "headers").(omap); ok {
		for _, entry := range headers {
			switch strings.ToLower(entry.key) {
			case "content-length", "transfer-encoding", "content-encoding":
				continue
			}
			if text, ok := entry.value.(string); ok {
				header.Set(entry.key, text)
			}
		}
	}
	status := 200
	if parsed, ok := numberValue(fieldOr(response, "status")); ok {
		status = int(parsed)
	}
	bodyBytes := responseBodyBytes(response)
	if stream, ok := fieldOr(response, "stream").(omap); ok {
		if chunks, ok := fieldOr(stream, "chunks").([]any); ok {
			if truncated, _ := fieldOr(stream, "truncated").(bool); truncated {
				// The capture kept the body but not every chunk boundary;
				// serving a guessed stream shape would be a silent lie.
				s.diverge("http",
					append(probe, omapEntry{"streamBoundariesTruncated", true}))
				return divergedResponse(request, "truncated-stream-boundaries"), nil
			}
			served := synthesizedResponse(request, status, header, nil)
			served.Body = &chunkReader{chunks: splitChunks(bodyBytes, chunks)}
			served.ContentLength = int64(len(bodyBytes))
			return served, nil
		}
	}
	return synthesizedResponse(request, status, header, bodyBytes), nil
}

// splitChunks splits a replayed body at the recorded chunk boundaries (byte
// lengths). Redaction can change body byte counts, so lengths are clamped
// and the last chunk absorbs any remainder: the CHUNK COUNT (the stream
// shape the app observed) is preserved exactly, the recorded content is
// never padded.
func splitChunks(body []byte, lengths []any) [][]byte {
	chunks := make([][]byte, 0, len(lengths))
	offset := 0
	for index, raw := range lengths {
		last := index == len(lengths)-1
		size := 0
		if parsed, ok := numberValue(raw); ok {
			size = int(parsed)
		}
		end := len(body)
		if !last {
			end = min(offset+size, len(body))
		}
		chunks = append(chunks, body[offset:end])
		offset = end
	}
	return chunks
}

// chunkReader re-serves a recorded stream chunk for chunk: every Read
// returns bytes from at most ONE recorded chunk, so the consumer observes
// the recorded boundaries.
type chunkReader struct {
	chunks [][]byte
	index  int
	offset int
}

func (r *chunkReader) Read(buffer []byte) (int, error) {
	for r.index < len(r.chunks) {
		chunk := r.chunks[r.index]
		if r.offset >= len(chunk) {
			r.index++
			r.offset = 0
			continue
		}
		read := copy(buffer, chunk[r.offset:])
		r.offset += read
		if r.offset >= len(chunk) {
			r.index++
			r.offset = 0
		}
		return read, nil
	}
	return 0, io.EOF
}

func (r *chunkReader) Close() error { return nil }

// responseBodyBytes renders the recorded body: a string is served verbatim,
// any other JSON value re-encodes exactly as the Node reference stringifies
// the same recorded value (insertion order preserved).
func responseBodyBytes(response omap) []byte {
	value := fieldOr(response, "body")
	if value == absent || value == nil {
		return nil
	}
	if text, ok := value.(string); ok {
		return []byte(text)
	}
	return nodeJSON(value)
}

// tryJSONOrdered decodes a declared-JSON body preserving key order, so a
// probe echoed into a divergence marker serializes byte-identically to the
// bytes the app sent. Anything else stays text.
func tryJSONOrdered(body []byte, contentType string) any {
	if strings.Contains(contentType, "application/json") {
		if decoded, err := decodeOrderedJSON(body); err == nil {
			return decoded
		}
	}
	return string(body)
}

func divergedResponse(request *http.Request, reason string) *http.Response {
	header := http.Header{}
	header.Set("Content-Type", "application/json")
	body := nodeJSON(omap{{"reproit", reason}})
	return synthesizedResponse(request, 599, header, body)
}

func synthesizedResponse(
	request *http.Request,
	status int,
	header http.Header,
	body []byte,
) *http.Response {
	return &http.Response{
		Status:        strconv.Itoa(status) + " " + http.StatusText(status),
		StatusCode:    status,
		Proto:         "HTTP/1.1",
		ProtoMajor:    1,
		ProtoMinor:    1,
		Header:        header,
		Body:          io.NopCloser(bytes.NewReader(body)),
		ContentLength: int64(len(body)),
		Request:       request,
	}
}

// serveDB answers one statement from the recording as the RunDB outcome
// shape. A divergence and a recorded failure both surface as errors, never
// as a silent empty result.
func (s *replaySession) serveDB(text string, values []any) (DBOutcome, error) {
	command, rowCount, rows, err := s.serveDBExchange(text, values)
	if err != nil {
		return DBOutcome{}, err
	}
	outcome := DBOutcome{Command: command, RowCount: rowCount}
	for _, row := range rows {
		outcome.Rows = append(outcome.Rows, plain(row))
	}
	return outcome, nil
}

// serveDBExchange is the ordered core shared by RunDB and the database/sql
// driver: rows keep their recorded column order so a driver can expose
// positional columns faithfully.
func (s *replaySession) serveDBExchange(
	text string,
	values []any,
) (command string, rowCount uint64, rows []any, err error) {
	probe := omap{{"text", text}}
	if len(values) > 0 {
		normalized, _ := normalize(values).([]any)
		probe = append(probe, omapEntry{"values", normalized})
	}
	recorded := s.matched("pg", probe)
	if recorded == nil {
		return "", 0, nil, &DBError{Message: "reproit: db call diverged from the capture"}
	}
	response, _ := fieldOr(recorded, "response").(omap)
	if failure, ok := fieldOr(response, "error").(omap); ok {
		message := fieldString(failure, "message")
		if message == "" {
			message = "recorded db error"
		}
		return "", 0, nil, &DBError{
			Message: message,
			Code:    fieldString(failure, "code"),
		}
	}
	command = fieldString(response, "command")
	rowCount, _ = numberValue(fieldOr(response, "rowCount"))
	rows, _ = fieldOr(response, "rows").([]any)
	return command, rowCount, rows, nil
}

// httpRequestMatches compares method, path+query of the original URL, and
// body modulo redaction placeholders. Recorded headers are deliberately not
// matched: they carry per-run noise (dates, connection management) that
// would turn every replay into a divergence. Host and scheme are not
// compared either: a replayed app dials a different origin.
func httpRequestMatches(recorded, probe any) bool {
	if recorded == nil || recorded == absent {
		return false
	}
	if fieldString(recorded, "method") != fieldString(probe, "method") {
		return false
	}
	if urlPathAndQuery(fieldString(recorded, "url")) !=
		urlPathAndQuery(fieldString(probe, "url")) {
		return false
	}
	body := fieldOr(recorded, "body")
	if body == absent {
		return true
	}
	live := fieldOr(probe, "body")
	if live == absent {
		live = nil
	}
	return matchesRecorded(body, live)
}

// dbRequestMatches compares exact statement text and values modulo
// placeholders.
func dbRequestMatches(recorded, probe any) bool {
	if recorded == nil || recorded == absent {
		return false
	}
	if fieldString(recorded, "text") != fieldString(probe, "text") {
		return false
	}
	values := fieldOr(recorded, "values")
	if values == absent {
		return true
	}
	live := fieldOr(probe, "values")
	if live == absent {
		live = nil
	}
	return matchesRecorded(values, live)
}

// matchesRecorded reports whether a live value satisfies the recorded one: a
// recorded nil matches anything, a `$reproit` placeholder matches any value
// that stood at its position, objects compare per key, and arrays compare
// elementwise. Ordered and plain objects are interchangeable here.
func matchesRecorded(recorded, live any) bool {
	switch value := recorded.(type) {
	case nil:
		return true
	case omap:
		if _, redacted := value.get("$reproit"); redacted {
			return true
		}
		if !isObject(live) {
			return false
		}
		for _, entry := range value {
			item := fieldOr(live, entry.key)
			if item == absent {
				item = nil
			}
			if !matchesRecorded(entry.value, item) {
				return false
			}
		}
		return true
	case map[string]any:
		if _, redacted := value["$reproit"]; redacted {
			return true
		}
		if !isObject(live) {
			return false
		}
		for key, item := range value {
			liveItem := fieldOr(live, key)
			if liveItem == absent {
				liveItem = nil
			}
			if !matchesRecorded(item, liveItem) {
				return false
			}
		}
		return true
	case []any:
		liveList, ok := live.([]any)
		if !ok || len(liveList) != len(value) {
			return false
		}
		for index, item := range value {
			if !matchesRecorded(item, liveList[index]) {
				return false
			}
		}
		return true
	case json.Number:
		liveNumber, ok := live.(json.Number)
		return ok && value.String() == liveNumber.String()
	default:
		return recorded == live
	}
}

func isObject(value any) bool {
	switch value.(type) {
	case omap, map[string]any:
		return true
	default:
		return false
	}
}

// numberValue reads a JSON number regardless of the decoder that produced it.
func numberValue(value any) (uint64, bool) {
	switch typed := value.(type) {
	case json.Number:
		parsed, err := strconv.ParseUint(typed.String(), 10, 64)
		return parsed, err == nil
	case float64:
		if typed < 0 {
			return 0, false
		}
		return uint64(typed), true
	case int:
		if typed < 0 {
			return 0, false
		}
		return uint64(typed), true
	default:
		return 0, false
	}
}
