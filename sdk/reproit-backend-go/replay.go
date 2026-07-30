// Hermetic replay for reproit-backend-go.
//
// When REPROIT_REPLAY names a `reproit-backend-capture` payload, the same
// boundaries that record exchanges at capture time SERVE them instead, so the
// application re-executes against exactly what production saw with no live
// dependencies.
//
// Determinism is a contract here, not a similarity score. Matching is strict:
// the next unconsumed exchange of the same protocol is the only candidate,
// and recorded `$reproit` redaction placeholders match any value at their
// position. The first unmatched call is a DIVERGENCE: reported as a
// structured `REPROIT:DIVERGENCE` stderr line, then answered 599 (HTTP) or an
// error (db), never a fuzzy match.
//
// The envelope pins the replay's determinism: TZ comes from the capture and
// ReplayRNG yields the seeded stream. Honesty note: the seed makes REPLAY
// runs deterministic; it does not reproduce the randomness the app drew in
// production.
package reproitbackend

import (
	"bytes"
	"encoding/json"
	"io"
	"net/http"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"
)

// DivergenceMarker prefixes the structured divergence line, byte-identical
// to the Node and Rust SDKs'.
const DivergenceMarker = "REPROIT:DIVERGENCE "

type exchangeEntry struct {
	exchange map[string]any
	consumed bool
}

type replaySession struct {
	envelope  map[string]any
	mu        sync.Mutex
	exchanges []*exchangeEntry
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
// unsupported payload disables replay rather than half-serving it.
func loadReplaySession(path string) *replaySession {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	var payload map[string]any
	if decoder.Decode(&payload) != nil {
		return nil
	}
	if format, _ := payload["format"].(string); format != CaptureFormat {
		return nil
	}
	version, ok := numberValue(payload["version"])
	if !ok || version < 1 || version > 2 {
		return nil
	}
	loaded := &replaySession{}
	if envelope, ok := payload["envelope"].(map[string]any); ok {
		loaded.envelope = envelope
	}
	events, _ := payload["events"].([]any)
	for _, item := range events {
		event, ok := item.(map[string]any)
		if !ok {
			continue
		}
		if kind, _ := event["kind"].(string); kind != "effect" {
			continue
		}
		exchange, ok := event["exchange"].(map[string]any)
		if !ok {
			continue
		}
		loaded.exchanges = append(loaded.exchanges, &exchangeEntry{exchange: exchange})
	}
	return loaded
}

// pinEnvelope applies the capture's timezone to this process. Go caches
// time.Local, so the location is replaced directly as well as through TZ.
func (s *replaySession) pinEnvelope() {
	tz, _ := s.envelope["tz"].(string)
	if strings.TrimSpace(tz) == "" {
		return
	}
	_ = os.Setenv("TZ", tz)
	if location, err := time.LoadLocation(tz); err == nil {
		time.Local = location
	}
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
	seed, _ := replay.envelope["replaySeed"].(string)
	if len(seed) == 0 {
		return nil
	}
	if len(seed) > 16 {
		seed = seed[:16]
	}
	state, err := strconv.ParseUint(seed, 16, 64)
	if err != nil {
		return nil
	}
	return &ReplayRNG{state: state | 1}
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

// matched applies the strict next-unconsumed rule: the first unconsumed
// exchange of the protocol is the ONLY candidate, because skipping it
// silently would be a fuzzy match. A nil result is a divergence, already
// reported.
func (s *replaySession) matched(protocol string, probe map[string]any) map[string]any {
	s.mu.Lock()
	var hit map[string]any
	for _, entry := range s.exchanges {
		if entry.consumed || protocolOf(entry.exchange) != protocol {
			continue
		}
		request, _ := entry.exchange["request"].(map[string]any)
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
	if hit != nil {
		s.mu.Unlock()
		return hit
	}
	var expected any
	consumed := 0
	for _, entry := range s.exchanges {
		if entry.consumed {
			consumed++
			continue
		}
		if expected == nil && protocolOf(entry.exchange) == protocol {
			expected = entry.exchange["request"]
		}
	}
	total := len(s.exchanges)
	s.mu.Unlock()
	s.diverge(protocol, probe, expected, consumed, total)
	return nil
}

func (s *replaySession) diverge(
	protocol string,
	probe map[string]any,
	expected any,
	consumed, total int,
) {
	report := map[string]any{
		"protocol": protocol,
		"got":      probe,
		"expected": expected,
		"consumed": json.Number(strconv.Itoa(consumed)),
		"total":    json.Number(strconv.Itoa(total)),
	}
	_, _ = os.Stderr.WriteString(DivergenceMarker + string(CanonicalJSON(report)) + "\n")
}

func protocolOf(exchange map[string]any) string {
	protocol, _ := exchange["protocol"].(string)
	return protocol
}

// serveHTTP answers one request entirely in process. A divergence and a
// truncated-at-capture body both serve a hard 599 so the application observes
// an attributable failure instead of a guess.
func (s *replaySession) serveHTTP(request *http.Request) (*http.Response, error) {
	body := []byte(nil)
	if request.Body != nil && request.Body != http.NoBody {
		body, _ = io.ReadAll(request.Body)
		_ = request.Body.Close()
	}
	probe := map[string]any{
		"method": request.Method,
		"url":    requestURL(request),
	}
	for key, value := range boundedBody(body, request.Header.Get("Content-Type")) {
		probe[key] = value
	}
	recorded := s.matched("http", probe)
	if recorded == nil {
		return divergedResponse(request, "diverged"), nil
	}
	response, _ := recorded["response"].(map[string]any)
	if truncated, _ := response["truncated"].(bool); truncated {
		// The capture kept identity but not bytes; serving a guessed body
		// would be a silent lie. Fail closed with the named reason.
		s.diverge("http", probe, recorded["request"], 0, 0)
		return divergedResponse(request, "truncated-exchange-body"), nil
	}
	header := http.Header{}
	if headers, ok := response["headers"].(map[string]any); ok {
		for name, value := range headers {
			switch strings.ToLower(name) {
			case "content-length", "transfer-encoding", "content-encoding":
				continue
			}
			if text, ok := value.(string); ok {
				header.Set(name, text)
			}
		}
	}
	status := 200
	if parsed, ok := numberValue(response["status"]); ok {
		status = int(parsed)
	}
	return synthesizedResponse(request, status, header, responseBodyBytes(response)), nil
}

// responseBodyBytes renders the recorded body: a string is served verbatim,
// any other JSON value is re-encoded.
func responseBodyBytes(response map[string]any) []byte {
	value, present := response["body"]
	if !present || value == nil {
		return nil
	}
	if text, ok := value.(string); ok {
		return []byte(text)
	}
	return CanonicalJSON(value)
}

func divergedResponse(request *http.Request, reason string) *http.Response {
	header := http.Header{}
	header.Set("Content-Type", "application/json")
	body := CanonicalJSON(map[string]any{"reproit": reason})
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

// serveDB answers one statement from the recording. A divergence and a
// recorded failure both surface as errors, never as a silent empty result.
func (s *replaySession) serveDB(text string, values []any) (DBOutcome, error) {
	probe := map[string]any{"text": text}
	if len(values) > 0 {
		probe["values"] = normalize(values)
	}
	recorded := s.matched("pg", probe)
	if recorded == nil {
		return DBOutcome{}, &DBError{Message: "reproit: db call diverged from the capture"}
	}
	response, _ := recorded["response"].(map[string]any)
	if failure, ok := response["error"].(map[string]any); ok {
		message, _ := failure["message"].(string)
		if message == "" {
			message = "recorded db error"
		}
		code, _ := failure["code"].(string)
		return DBOutcome{}, &DBError{Message: message, Code: code}
	}
	outcome := DBOutcome{}
	if command, ok := response["command"].(string); ok {
		outcome.Command = command
	}
	if rowCount, ok := numberValue(response["rowCount"]); ok {
		outcome.RowCount = rowCount
	}
	if rows, ok := response["rows"].([]any); ok {
		outcome.Rows = rows
	}
	return outcome, nil
}

// httpRequestMatches compares method, path+query of the original URL, and
// body modulo redaction placeholders. Recorded headers are deliberately not
// matched: they carry per-run noise (dates, connection management) that would
// turn every replay into a divergence.
func httpRequestMatches(recorded, probe map[string]any) bool {
	if recorded == nil {
		return false
	}
	if !sameString(recorded["method"], probe["method"]) {
		return false
	}
	recordedURL, _ := recorded["url"].(string)
	probeURL, _ := probe["url"].(string)
	if urlPathAndQuery(recordedURL) != urlPathAndQuery(probeURL) {
		return false
	}
	body, present := recorded["body"]
	if !present {
		return true
	}
	return matchesRecorded(body, probe["body"])
}

// dbRequestMatches compares exact statement text and values modulo
// placeholders.
func dbRequestMatches(recorded, probe map[string]any) bool {
	if recorded == nil {
		return false
	}
	if !sameString(recorded["text"], probe["text"]) {
		return false
	}
	values, present := recorded["values"]
	if !present {
		return true
	}
	return matchesRecorded(values, probe["values"])
}

func sameString(left, right any) bool {
	leftText, leftOK := left.(string)
	rightText, rightOK := right.(string)
	return leftOK && rightOK && leftText == rightText
}

// matchesRecorded reports whether a live value satisfies the recorded one: a
// recorded nil matches anything, a `$reproit` placeholder matches any value
// that stood at its position, objects compare per key, and arrays compare
// elementwise.
func matchesRecorded(recorded, live any) bool {
	switch value := recorded.(type) {
	case nil:
		return true
	case map[string]any:
		if _, redacted := value["$reproit"]; redacted {
			return true
		}
		liveMap, ok := live.(map[string]any)
		if !ok {
			return false
		}
		for key, item := range value {
			if !matchesRecorded(item, liveMap[key]) {
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
