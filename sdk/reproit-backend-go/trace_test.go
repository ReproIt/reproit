package reproitbackend

import (
	"encoding/base64"
	"encoding/json"
	"strings"
	"testing"
)

func headerGetter(values map[string]string) func(string) string {
	return func(name string) string { return values[name] }
}

func TestEmitsBoundedCorrelatedRedactedEvents(t *testing.T) {
	context := TraceContextFromHeaders(headerGetter(map[string]string{
		"x-reproit-trace":           "trace-a",
		"x-reproit-actor":           "alice",
		"x-reproit-action":          "7",
		"x-reproit-build":           "build-a",
		"x-reproit-config-contract": "contract-a",
	}))
	if context == nil {
		t.Fatal("expected a trace context")
	}
	selection := NewSelection("project.id", "projectId")
	if selection == nil {
		t.Fatal("expected a valid selection")
	}
	trace, err := Begin(context, "createProject", BeginOptions{
		Tenant:         "org-1",
		IdempotencyKey: "retry-secret",
		Input: map[string]any{
			"name":     "demo",
			"password": "abcdefgh",
		},
		Selections: []Selection{*selection},
	})
	if err != nil {
		t.Fatal(err)
	}
	err = trace.Effect(EffectWrite, EffectOptions{
		Resource: "projects", Key: "1", Tenant: "org-1",
	})
	if err != nil {
		t.Fatal(err)
	}
	err = trace.Finish(map[string]any{
		"id":          1,
		"apiKey":      "sk_live_secret",
		"private-key": "private-secret",
		"access key":  "access-secret",
		"signingKey":  "signing-secret",
		"monkey":      "harmless",
	}, 201, true, true)
	if err != nil {
		t.Fatal(err)
	}
	header, err := trace.Header()
	if err != nil || len(header) >= MaxHeaderBytes {
		t.Fatalf("header: %v (%d bytes)", err, len(header))
	}
	events := trace.Events()
	start := events[0]
	if start["actionIndex"] != json.Number("7") || start["build"] != "build-a" ||
		start["configContract"] != "contract-a" {
		t.Fatalf("start event correlation fields wrong: %v", start)
	}
	input := start["input"].(map[string]any)
	stub := input["password"].(map[string]any)["$reproit"].(map[string]any)
	if stub["redacted"] != true || stub["length"] != json.Number("8") {
		t.Fatalf("password not structurally redacted: %v", stub)
	}
	if start["idempotencyKey"] == "retry-secret" {
		t.Fatal("idempotency key shipped raw")
	}
	output := events[2]["output"].(map[string]any)
	for _, field := range []string{"apiKey", "private-key", "access key", "signingKey"} {
		value := output[field].(map[string]any)["$reproit"].(map[string]any)
		if value["redacted"] != true {
			t.Fatalf("%s not redacted", field)
		}
	}
	if output["monkey"] != "harmless" {
		t.Fatal("non-secret field damaged")
	}
	if events[2]["effectsComplete"] != true {
		t.Fatal("effectsComplete lost")
	}
}

func TestStaysInactiveWithoutATraceHeader(t *testing.T) {
	if TraceContextFromHeaders(headerGetter(nil)) != nil {
		t.Fatal("adapter must stay inert without x-reproit-trace")
	}
}

func TestOneReturnAndNoEffectsAfterReturn(t *testing.T) {
	trace, err := Begin(&TraceContext{TraceID: "t"}, "op", BeginOptions{})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := trace.Header(); err != ErrAlreadyFinished {
		t.Fatalf("header before finish: %v", err)
	}
	if err := trace.Finish(nil, 200, true, false); err != nil {
		t.Fatal(err)
	}
	if err := trace.Finish(nil, 200, true, false); err != ErrAlreadyFinished {
		t.Fatalf("second finish: %v", err)
	}
	err = trace.Effect(EffectRead, EffectOptions{})
	if err != ErrAlreadyFinished {
		t.Fatalf("effect after return: %v", err)
	}
	if err := trace.Effect("smash", EffectOptions{}); err != ErrInvalidOperation {
		t.Fatalf("untyped effect: %v", err)
	}
}

func TestEventCountAndIdentifierBounds(t *testing.T) {
	if _, err := Begin(&TraceContext{TraceID: "t"}, "  ", BeginOptions{}); err == nil {
		t.Fatal("blank operation accepted")
	}
	long := strings.Repeat("x", 257)
	if _, err := Begin(&TraceContext{TraceID: "t"}, long, BeginOptions{}); err == nil {
		t.Fatal("overlong operation accepted")
	}
	trace, err := Begin(&TraceContext{TraceID: "t"}, "op", BeginOptions{})
	if err != nil {
		t.Fatal(err)
	}
	for range MaxEvents - 1 {
		if err := trace.Effect(EffectRead, EffectOptions{}); err != nil {
			t.Fatal(err)
		}
	}
	if err := trace.Effect(EffectRead, EffectOptions{}); err != ErrTooManyEvents {
		t.Fatalf("event bound not enforced: %v", err)
	}
}

func TestCanonicalHTTPInputLowercasesHeaders(t *testing.T) {
	value := HTTPInput{
		Body:    map[string]any{"name": "demo"},
		Path:    map[string]any{"project": "p1"},
		Query:   map[string]any{"tag": []any{"a", "b"}},
		Headers: map[string]any{"X-Mode": "safe"},
	}.Value()
	headers := value["headers"].(map[string]any)
	if headers["x-mode"] != "safe" {
		t.Fatalf("headers not lowercased: %v", headers)
	}
	tags := value["query"].(map[string]any)["tag"].([]any)
	if len(tags) != 2 {
		t.Fatalf("repeated query values lost: %v", tags)
	}
}

// Goldens produced by the Node adapter's canonicalJson: the ports are pinned
// byte-identical (serde_json BTreeMap order, compact separators).
const goldenValueJSON = `{"a":{"num":2.5,"quo\"te":"line\nbreak","z":[1,"two",true,null]},` +
	`"b":1,"big":9007199254740991,"emptyArr":[],"emptyObj":{},"neg":-42,"uni":"héllo  "}`

const goldenEventsJSON = `[{"actionIndex":3,"actor":"alice","build":"b1",` +
	`"idempotencyKey":"sha256:691a2bdae9040f9fcfe6ff3f",` +
	`"input":{"item":"widget",` +
	`"password":{"$reproit":{"length":8,"redacted":true,"type":"string"}}},` +
	`"kind":"start","operation":"createOrder","spanId":"trace-g:createOrder",` +
	`"tenant":"org-1","traceId":"trace-g"},` +
	`{"actionIndex":3,"actor":"alice","build":"b1","effect":"write",` +
	`"idempotencyKey":"sha256:691a2bdae9040f9fcfe6ff3f","key":"1","kind":"effect",` +
	`"operation":"createOrder","resource":"orders","spanId":"trace-g:createOrder",` +
	`"tenant":"org-1","traceId":"trace-g"},` +
	`{"actionIndex":3,"actor":"alice","build":"b1","effectsComplete":true,` +
	`"idempotencyKey":"sha256:691a2bdae9040f9fcfe6ff3f","kind":"return",` +
	`"operation":"createOrder",` +
	`"output":{"apiKey":{"$reproit":{"length":9,"redacted":true,"type":"string"}},` +
	`"ok":true},"spanId":"trace-g:createOrder","status":201,"success":true,` +
	`"tenant":"org-1","traceId":"trace-g"}]`

func TestCanonicalJSONMatchesTheNodeGolden(t *testing.T) {
	value := map[string]any{
		"b": 1,
		"a": map[string]any{
			"z":       []any{1, "two", true, nil},
			"quo\"te": "line\nbreak",
			"num":     2.5,
		},
		"emptyObj": map[string]any{},
		"emptyArr": []any{},
		"big":      int64(9007199254740991),
		"neg":      -42,
		"uni":      "héllo  ",
	}
	if got := string(CanonicalJSON(value)); got != goldenValueJSON {
		t.Fatalf("canonical JSON diverged from the Node golden:\n got %s\nwant %s",
			got, goldenValueJSON)
	}
}

func TestTraceEventBytesMatchTheNodeGolden(t *testing.T) {
	context := &TraceContext{TraceID: "trace-g", Actor: "alice", ActionIndex: 3, Build: "b1"}
	trace, err := Begin(context, "createOrder", BeginOptions{
		Tenant:         "org-1",
		IdempotencyKey: "retry-secret",
		Input:          map[string]any{"item": "widget", "password": "hunter22"},
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := trace.Effect(EffectWrite, EffectOptions{Resource: "orders", Key: "1"}); err != nil {
		t.Fatal(err)
	}
	err = trace.Finish(map[string]any{"ok": true, "apiKey": "sk_live_x"}, 201, true, true)
	if err != nil {
		t.Fatal(err)
	}
	events := make([]any, 0, 3)
	for _, event := range trace.Events() {
		copied := make(map[string]any, len(event))
		for key, value := range event {
			if key != "sequence" {
				copied[key] = value
			}
		}
		events = append(events, copied)
	}
	if got := string(CanonicalJSON(events)); got != goldenEventsJSON {
		t.Fatalf("event bytes diverged from the Node golden:\n got %s\nwant %s",
			got, goldenEventsJSON)
	}
	header, err := trace.Header()
	if err != nil {
		t.Fatal(err)
	}
	decoded, err := base64.RawURLEncoding.DecodeString(header)
	if err != nil {
		t.Fatalf("header is not unpadded base64url: %v", err)
	}
	var roundTrip []map[string]any
	if err := json.Unmarshal(decoded, &roundTrip); err != nil || len(roundTrip) != 3 {
		t.Fatalf("header does not decode to the events: %v", err)
	}
}

func TestSelectionValidation(t *testing.T) {
	if NewSelection("bad path", "ok") != nil {
		t.Fatal("invalid schema path accepted")
	}
	if NewSelection("a.b[]", "c").WithTypeCondition("Ty.pe") != nil {
		t.Fatal("dotted type condition accepted")
	}
	if NewSelection("a.b[]", "c").WithTypeCondition("Type") == nil {
		t.Fatal("valid selection rejected")
	}
}
