// Package reproitbackend is the experimental Go backend trace adapter.
//
// Go port of sdk/reproit-backend-rs. Scan-time: services activate this
// adapter only when a trusted request carries `x-reproit-trace`. The
// resulting response header (`x-reproit-events`) contains bounded,
// trace-bound, structurally redacted events. Production: the optional,
// config-gated capture mode (capture.go) self-samples finished traces
// (always on 5xx / failure, optional healthy baseline) and posts them to
// Cloud ingest. It is not a public compatibility surface while backend
// contracts remain experimental.
//
// Wire parity with the Rust adapter: events serialize as compact JSON with
// recursively sorted keys (serde_json's BTreeMap order), and the header is
// unpadded base64url of that encoding. Zero third-party dependencies.
package reproitbackend

import (
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"sort"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"unicode/utf8"
)

const (
	// MaxEvents bounds the events one trace may hold (start + effects + return).
	MaxEvents = 256
	// MaxHeaderBytes bounds the encoded `x-reproit-events` response header.
	MaxHeaderBytes = 60000
)

// Trace rejection reasons, mirroring the Rust TraceError variants.
var (
	ErrInvalidOperation = errors.New("reproit trace rejected input: InvalidOperation")
	ErrAlreadyFinished  = errors.New("reproit trace rejected input: AlreadyFinished")
	ErrTooManyEvents    = errors.New("reproit trace rejected input: TooManyEvents")
	ErrHeaderTooLarge   = errors.New("reproit trace rejected input: HeaderTooLarge")
)

// EffectKind is the closed set of typed effects a handler may record.
type EffectKind string

const (
	EffectRead   EffectKind = "read"
	EffectWrite  EffectKind = "write"
	EffectDelete EffectKind = "delete"
	EffectEmit   EffectKind = "emit"
	EffectCall   EffectKind = "call"
)

var sequenceCounter atomic.Uint64

func init() { sequenceCounter.Store(1) }

// TraceContext identifies the trusted scan-time trace (or a synthesized
// capture-mode context). Empty strings mean absent.
type TraceContext struct {
	TraceID        string
	Actor          string
	ActionIndex    uint32
	Build          string
	ConfigContract string
}

// TraceContextFromHeaders builds a context from a request header lookup
// (empty string means the header is missing). Returns nil when no valid
// `x-reproit-trace` is present: the adapter stays inert.
func TraceContextFromHeaders(get func(name string) string) *TraceContext {
	traceID, ok := bounded(get("x-reproit-trace"), 128)
	if !ok {
		return nil
	}
	header := func(name string, maximum int) string {
		value, _ := bounded(get(name), maximum)
		return value
	}
	actionIndex := uint32(0)
	if raw := strings.TrimSpace(get("x-reproit-action")); raw != "" {
		if parsed, err := strconv.ParseUint(raw, 10, 32); err == nil {
			actionIndex = uint32(parsed)
		}
	}
	return &TraceContext{
		TraceID:        traceID,
		Actor:          header("x-reproit-actor", 32),
		ActionIndex:    actionIndex,
		Build:          header("x-reproit-build", 128),
		ConfigContract: header("x-reproit-config-contract", 128),
	}
}

// Selection is a GraphQL selection mapping (parser-produced only).
type Selection struct {
	SchemaPath    string
	ResponsePath  string
	TypeCondition string
}

// NewSelection returns nil on an invalid path, matching the Rust constructor.
func NewSelection(schemaPath, responsePath string) *Selection {
	if !validPath(schemaPath) || !validPath(responsePath) {
		return nil
	}
	return &Selection{SchemaPath: schemaPath, ResponsePath: responsePath}
}

// WithTypeCondition returns nil when the condition is not a bare valid name.
func (s *Selection) WithTypeCondition(condition string) *Selection {
	if s == nil || !validPath(condition) ||
		strings.Contains(condition, ".") || strings.Contains(condition, "[]") {
		return nil
	}
	out := *s
	out.TypeCondition = condition
	return &out
}

func (s Selection) value() map[string]any {
	value := map[string]any{
		"schemaPath":   s.SchemaPath,
		"responsePath": s.ResponsePath,
	}
	if s.TypeCondition != "" {
		value["typeCondition"] = s.TypeCondition
	}
	return value
}

// HTTPInput is the canonical decoded OpenAPI input. Framework adapters must
// provide decoded values (including slices for repeated query/header
// parameters), never raw query strings whose serialization is ambiguous.
type HTTPInput struct {
	Body    any
	Path    map[string]any
	Query   map[string]any
	Headers map[string]any
}

// Value produces the canonical start-event input object.
func (in HTTPInput) Value() map[string]any {
	value := map[string]any{}
	if in.Body != nil {
		value["body"] = in.Body
	}
	for _, part := range []struct {
		name   string
		fields map[string]any
	}{{"path", in.Path}, {"query", in.Query}, {"headers", in.Headers}} {
		if len(part.fields) == 0 {
			continue
		}
		fields := make(map[string]any, len(part.fields))
		for key, field := range part.fields {
			if part.name == "headers" {
				key = strings.ToLower(key)
			}
			fields[key] = field
		}
		value[part.name] = fields
	}
	return value
}

// BeginOptions carries the optional Begin parameters. Empty strings mean
// absent; IdempotencyKey is hashed before it enters any event.
type BeginOptions struct {
	SpanID         string
	Tenant         string
	IdempotencyKey string
	Input          any
	Selections     []Selection
}

// EffectOptions carries the optional Effect parameters. Detail must be an
// object; only its (redacted) before/after/payload fields are kept.
type EffectOptions struct {
	Resource string
	Key      string
	Tenant   string
	Event    string
	Detail   any
}

// BackendTrace records one operation as bounded, redacted events. Safe for
// concurrent use by the request goroutine and the adapter.
type BackendTrace struct {
	mu       sync.Mutex
	common   map[string]any
	events   []map[string]any
	finished bool
}

// Begin starts an operation trace with a redacted canonical start event.
func Begin(context *TraceContext, operation string, opts BeginOptions) (*BackendTrace, error) {
	if context == nil {
		return nil, ErrInvalidOperation
	}
	name, ok := bounded(operation, 256)
	if !ok {
		return nil, ErrInvalidOperation
	}
	spanID := opts.SpanID
	if spanID == "" {
		spanID = context.TraceID + ":" + name
	}
	spanID, ok = bounded(spanID, 128)
	if !ok {
		return nil, ErrInvalidOperation
	}
	common := map[string]any{
		"traceId":     context.TraceID,
		"spanId":      spanID,
		"actionIndex": json.Number(strconv.FormatUint(uint64(context.ActionIndex), 10)),
		"operation":   name,
	}
	if context.Actor != "" {
		common["actor"] = context.Actor
	}
	if context.Build != "" {
		common["build"] = context.Build
	}
	if context.ConfigContract != "" {
		common["configContract"] = context.ConfigContract
	}
	if tenant, ok := bounded(opts.Tenant, 128); ok {
		common["tenant"] = tenant
	}
	if opts.IdempotencyKey != "" {
		common["idempotencyKey"] = identity(opts.IdempotencyKey)
	}
	if len(opts.Selections) > 0 {
		selections := opts.Selections
		if len(selections) > MaxEvents {
			selections = selections[:MaxEvents]
		}
		values := make([]any, 0, len(selections))
		for _, selection := range selections {
			values = append(values, selection.value())
		}
		common["selections"] = values
	}
	trace := &BackendTrace{common: common}
	err := trace.push("start", map[string]any{"input": redact(normalize(opts.Input))})
	if err != nil {
		return nil, err
	}
	return trace, nil
}

// Effect records one observed effect. Fails after finish.
func (t *BackendTrace) Effect(kind EffectKind, opts EffectOptions) error {
	switch kind {
	case EffectRead, EffectWrite, EffectDelete, EffectEmit, EffectCall:
	default:
		return ErrInvalidOperation
	}
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.finished {
		return ErrAlreadyFinished
	}
	fields := map[string]any{"effect": string(kind)}
	for _, part := range []struct{ name, value string }{
		{"resource", opts.Resource},
		{"key", opts.Key},
		{"effectTenant", opts.Tenant},
		{"event", opts.Event},
	} {
		if part.value != "" {
			fields[part.name] = truncate(part.value, 256)
		}
	}
	if opts.Detail != nil {
		if detail, ok := redact(normalize(opts.Detail)).(map[string]any); ok {
			for _, key := range []string{"before", "after", "payload"} {
				if value, present := detail[key]; present {
					fields[key] = value
				}
			}
		}
	}
	return t.push("effect", fields)
}

// Finish records the single return event. A second finish fails.
func (t *BackendTrace) Finish(output any, status int, success, effectsComplete bool) error {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.finished {
		return ErrAlreadyFinished
	}
	err := t.push("return", map[string]any{
		"output":          redact(normalize(output)),
		"status":          json.Number(strconv.Itoa(status)),
		"success":         success,
		"effectsComplete": effectsComplete,
	})
	if err != nil {
		return err
	}
	t.finished = true
	return nil
}

// Header encodes the finished trace as the `x-reproit-events` value:
// unpadded base64url over canonical JSON, bounded at MaxHeaderBytes.
func (t *BackendTrace) Header() (string, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	if !t.finished {
		return "", ErrAlreadyFinished
	}
	encoded := base64.RawURLEncoding.EncodeToString(CanonicalJSON(t.eventsLocked()))
	if len(encoded) > MaxHeaderBytes {
		return "", ErrHeaderTooLarge
	}
	return encoded, nil
}

// Events returns a snapshot of the recorded events (event maps are shared;
// treat them as read-only).
func (t *BackendTrace) Events() []map[string]any {
	t.mu.Lock()
	defer t.mu.Unlock()
	return append([]map[string]any(nil), t.events...)
}

// Finished reports whether the return event has been recorded.
func (t *BackendTrace) Finished() bool {
	t.mu.Lock()
	defer t.mu.Unlock()
	return t.finished
}

func (t *BackendTrace) eventsLocked() []any {
	events := make([]any, 0, len(t.events))
	for _, event := range t.events {
		events = append(events, event)
	}
	return events
}

// push appends one event under the caller-held lock (Begin holds no lock but
// owns the sole reference).
func (t *BackendTrace) push(kind string, fields map[string]any) error {
	if len(t.events) >= MaxEvents {
		return ErrTooManyEvents
	}
	event := make(map[string]any, len(t.common)+len(fields)+2)
	for key, value := range t.common {
		event[key] = value
	}
	sequence := sequenceCounter.Add(1) - 1
	event["sequence"] = json.Number(strconv.FormatUint(sequence, 10))
	event["kind"] = kind
	for key, value := range fields {
		event[key] = value
	}
	t.events = append(t.events, event)
	return nil
}

// bounded trims and enforces a rune-count bound; ok is false when empty.
func bounded(value string, maximum int) (string, bool) {
	value = strings.TrimSpace(value)
	if value == "" || utf8.RuneCountInString(value) > maximum {
		return "", false
	}
	return value, true
}

func truncate(value string, maximum int) string {
	runes := []rune(value)
	if len(runes) <= maximum {
		return value
	}
	return string(runes[:maximum])
}

func validPath(path string) bool {
	if path == "" {
		return false
	}
	for _, segment := range strings.Split(path, ".") {
		name := strings.TrimSuffix(segment, "[]")
		if name == "" {
			return false
		}
		for index, ch := range name {
			letter := ch == '_' || (ch >= 'a' && ch <= 'z') || (ch >= 'A' && ch <= 'Z')
			if index == 0 && !letter {
				return false
			}
			if !letter && !(ch >= '0' && ch <= '9') {
				return false
			}
		}
	}
	return true
}

// identity hashes idempotency keys: never ship the raw key.
func identity(value string) string {
	digest := sha256.Sum256([]byte(value))
	return "sha256:" + hex.EncodeToString(digest[:12])
}

var secretParts = []string{
	"password", "passwd", "secret", "token", "authorization", "cookie",
	"email", "phone", "apikey", "publishablekey", "privatekey", "accesskey",
	"signingkey", "idempotencykey",
}

func secretField(name string) bool {
	var folded strings.Builder
	for _, ch := range name {
		switch {
		case ch >= 'A' && ch <= 'Z':
			folded.WriteRune(ch + ('a' - 'A'))
		case (ch >= 'a' && ch <= 'z') || (ch >= '0' && ch <= '9'):
			folded.WriteRune(ch)
		}
	}
	for _, part := range secretParts {
		if strings.Contains(folded.String(), part) {
			return true
		}
	}
	return false
}

// redact applies recursive structural redaction to a normalized value:
// secret-named fields are replaced with a `$reproit` metadata stub
// (type + length), everything else recurses.
func redact(value any) any {
	switch v := value.(type) {
	case []any:
		out := make([]any, len(v))
		for index, item := range v {
			out[index] = redact(item)
		}
		return out
	case map[string]any:
		out := make(map[string]any, len(v))
		for key, field := range v {
			if secretField(key) {
				out[key] = metadata(field)
			} else {
				out[key] = redact(field)
			}
		}
		return out
	default:
		return value
	}
}

func metadata(value any) map[string]any {
	kind := "null"
	var length any
	switch v := value.(type) {
	case bool:
		kind = "boolean"
	case json.Number:
		kind = "number"
		if !strings.ContainsAny(v.String(), ".eE") {
			kind = "integer"
		}
	case string:
		kind = "string"
		length = json.Number(strconv.Itoa(utf8.RuneCountInString(v)))
	case []any:
		kind = "array"
		length = json.Number(strconv.Itoa(len(v)))
	case map[string]any:
		kind = "object"
	}
	return map[string]any{"$reproit": map[string]any{
		"redacted": true,
		"type":     kind,
		"length":   length,
	}}
}

// normalize restricts a value to nil/bool/string/json.Number/[]any/
// map[string]any so redaction and canonical encoding see one shape.
// Unrepresentable values normalize to nil (fail closed, never error).
func normalize(value any) any {
	switch v := value.(type) {
	case nil, bool, string, json.Number:
		return v
	case int:
		return json.Number(strconv.Itoa(v))
	case int32:
		return json.Number(strconv.FormatInt(int64(v), 10))
	case int64:
		return json.Number(strconv.FormatInt(v, 10))
	case uint32:
		return json.Number(strconv.FormatUint(uint64(v), 10))
	case uint64:
		return json.Number(strconv.FormatUint(v, 10))
	case []any:
		out := make([]any, len(v))
		for index, item := range v {
			out[index] = normalize(item)
		}
		return out
	case map[string]any:
		out := make(map[string]any, len(v))
		for key, field := range v {
			out[key] = normalize(field)
		}
		return out
	default:
		// Floats, structs, typed maps/slices: round-trip through
		// encoding/json (number literals preserved via json.Number).
		encoded, err := json.Marshal(v)
		if err != nil {
			return nil
		}
		decoder := json.NewDecoder(strings.NewReader(string(encoded)))
		decoder.UseNumber()
		var decoded any
		if decoder.Decode(&decoded) != nil {
			return nil
		}
		return decoded
	}
}

// CanonicalJSON encodes a normalized value as compact JSON with recursively
// sorted object keys: byte-identical to the Rust adapter's serde_json
// (BTreeMap) encoding of the same events.
func CanonicalJSON(value any) []byte {
	return appendCanonical(nil, normalize(value))
}

func appendCanonical(dst []byte, value any) []byte {
	switch v := value.(type) {
	case nil:
		return append(dst, "null"...)
	case bool:
		if v {
			return append(dst, "true"...)
		}
		return append(dst, "false"...)
	case json.Number:
		return append(dst, v.String()...)
	case string:
		return appendJSONString(dst, v)
	case []any:
		dst = append(dst, '[')
		for index, item := range v {
			if index > 0 {
				dst = append(dst, ',')
			}
			dst = appendCanonical(dst, item)
		}
		return append(dst, ']')
	case map[string]any:
		keys := make([]string, 0, len(v))
		for key := range v {
			keys = append(keys, key)
		}
		sort.Strings(keys)
		dst = append(dst, '{')
		for index, key := range keys {
			if index > 0 {
				dst = append(dst, ',')
			}
			dst = appendJSONString(dst, key)
			dst = append(dst, ':')
			dst = appendCanonical(dst, v[key])
		}
		return append(dst, '}')
	default:
		// normalize() guarantees the cases above; fail closed regardless.
		return append(dst, "null"...)
	}
}

// appendJSONString escapes exactly like serde_json and JSON.stringify:
// quote, backslash, and control shortcuts \b \t \n \f \r, with \u00XX for
// the remaining control characters. Everything else is written verbatim.
func appendJSONString(dst []byte, value string) []byte {
	const hexDigits = "0123456789abcdef"
	dst = append(dst, '"')
	for _, ch := range []byte(value) {
		switch {
		case ch == '"':
			dst = append(dst, '\\', '"')
		case ch == '\\':
			dst = append(dst, '\\', '\\')
		case ch == '\b':
			dst = append(dst, '\\', 'b')
		case ch == '\t':
			dst = append(dst, '\\', 't')
		case ch == '\n':
			dst = append(dst, '\\', 'n')
		case ch == '\f':
			dst = append(dst, '\\', 'f')
		case ch == '\r':
			dst = append(dst, '\\', 'r')
		case ch < 0x20:
			dst = append(dst, '\\', 'u', '0', '0', hexDigits[ch>>4], hexDigits[ch&0xf])
		default:
			dst = append(dst, ch)
		}
	}
	return append(dst, '"')
}
