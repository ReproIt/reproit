package reproitbackend

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// tracedRequest runs one round trip through the Transport with an ambient
// capture-mode trace and returns the trace's recorded events.
func tracedRequest(t *testing.T, server *httptest.Server, body string) *BackendTrace {
	t.Helper()
	trace, err := Begin(&TraceContext{TraceID: "cap-x-1", CaptureEnvelope: true}, "GET /quote",
		BeginOptions{Input: HTTPInput{Query: map[string]any{"symbol": "ACME"}}.Value()})
	if err != nil {
		t.Fatal(err)
	}
	client := WrapClient(server.Client())
	method := http.MethodGet
	var reader io.Reader
	if body != "" {
		method = http.MethodPost
		reader = strings.NewReader(body)
	}
	request, err := http.NewRequest(method, server.URL+"/prices?tier=gold", reader)
	if err != nil {
		t.Fatal(err)
	}
	if body != "" {
		request.Header.Set("Content-Type", "application/json")
	}
	request = request.WithContext(ContextWithTrace(request.Context(), trace))
	response, err := client.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := io.ReadAll(response.Body); err != nil {
		t.Fatal(err)
	}
	if err := response.Body.Close(); err != nil {
		t.Fatal(err)
	}
	return trace
}

func exchangeOf(t *testing.T, trace *BackendTrace) map[string]any {
	t.Helper()
	for _, event := range trace.Events() {
		if exchange, ok := event["exchange"].(map[string]any); ok {
			return exchange
		}
	}
	t.Fatal("no exchange recorded on the trace")
	return nil
}

func TestTransportRecordsRequestAndResponse(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(
		func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{"prices":[1,2],"apiKey":"sk-live-secret"}`))
		}))
	defer server.Close()

	exchange := exchangeOf(t, tracedRequest(t, server, `{"amount":5,"password":"hunter22"}`))
	if exchange["protocol"] != "http" {
		t.Fatalf("protocol wrong: %v", exchange)
	}
	request := exchange["request"].(map[string]any)
	if request["method"] != http.MethodPost {
		t.Fatalf("request method wrong: %v", request)
	}
	requestBody := request["body"].(map[string]any)
	if number, _ := requestBody["amount"].(json.Number); number.String() != "5" {
		t.Fatalf("request body lost: %v", requestBody)
	}
	// Structural redaction applies INSIDE captured exchange bodies.
	if secret, ok := requestBody["password"].(map[string]any); !ok ||
		secret["$reproit"] == nil {
		t.Fatalf("request body secret not redacted: %v", requestBody)
	}
	response := exchange["response"].(map[string]any)
	if status, _ := response["status"].(json.Number); status.String() != "200" {
		t.Fatalf("response status wrong: %v", response)
	}
	responseBody := response["body"].(map[string]any)
	prices := responseBody["prices"].([]any)
	if len(prices) != 2 {
		t.Fatalf("response body lost: %v", responseBody)
	}
	if secret, ok := responseBody["apiKey"].(map[string]any); !ok ||
		secret["$reproit"] == nil {
		t.Fatalf("response body secret not redacted: %v", responseBody)
	}
	if _, ok := response["headers"].(map[string]any); !ok {
		t.Fatalf("response headers not recorded: %v", response)
	}
}

func TestOversizedBodyKeepsProvableIdentityOnly(t *testing.T) {
	oversized := strings.Repeat("x", MaxExchangeBodyBytes+1)
	server := httptest.NewServer(http.HandlerFunc(
		func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Type", "text/plain")
			_, _ = w.Write([]byte(oversized))
		}))
	defer server.Close()

	exchange := exchangeOf(t, tracedRequest(t, server, ""))
	response := exchange["response"].(map[string]any)
	if truncated, _ := response["truncated"].(bool); !truncated {
		t.Fatalf("oversized body not marked truncated: %v", response)
	}
	if _, present := response["body"]; present {
		t.Fatalf("oversized body retained bytes: %v", response)
	}
	if bytesRecorded, _ := response["bodyBytes"].(json.Number); bytesRecorded.String() !=
		"8193" {
		t.Fatalf("oversized body byte count wrong: %v", response)
	}
	digest := sha256.Sum256([]byte(oversized))
	if response["bodySha256"] != hex.EncodeToString(digest[:]) {
		t.Fatalf("oversized body digest wrong: %v", response)
	}
}

func TestTransportWithoutAmbientTraceRecordsNothing(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(
		func(w http.ResponseWriter, r *http.Request) {
			_, _ = w.Write([]byte("{}"))
		}))
	defer server.Close()

	trace, err := Begin(&TraceContext{TraceID: "cap-x-2"}, "GET /quote", BeginOptions{})
	if err != nil {
		t.Fatal(err)
	}
	// No ContextWithTrace: the call is outside any traced request.
	response, err := WrapClient(server.Client()).Get(server.URL + "/prices")
	if err != nil {
		t.Fatal(err)
	}
	_, _ = io.ReadAll(response.Body)
	_ = response.Body.Close()
	for _, event := range trace.Events() {
		if _, ok := event["exchange"]; ok {
			t.Fatal("an untraced call recorded an exchange")
		}
	}
}

func TestRunDBRecordsRowsAndErrors(t *testing.T) {
	trace, err := Begin(&TraceContext{TraceID: "cap-x-3", CaptureEnvelope: true},
		"GET /quote", BeginOptions{})
	if err != nil {
		t.Fatal(err)
	}
	ctx := ContextWithTrace(t.Context(), trace)
	_, err = RunDB(ctx, "SELECT id FROM issuers WHERE symbol = $1", []any{"ACME"},
		func() (DBOutcome, error) {
			return DBOutcome{
				Command: "SELECT", RowCount: 1,
				Rows: []any{map[string]any{"id": 7}},
			}, nil
		})
	if err != nil {
		t.Fatal(err)
	}
	_, err = RunDB(ctx, "SELECT boom", nil, func() (DBOutcome, error) {
		return DBOutcome{}, &DBError{Message: "relation missing", Code: "42P01"}
	})
	if err == nil {
		t.Fatal("live db error was swallowed")
	}
	exchanges := []map[string]any{}
	for _, event := range trace.Events() {
		if exchange, ok := event["exchange"].(map[string]any); ok {
			exchanges = append(exchanges, exchange)
		}
	}
	if len(exchanges) != 2 {
		t.Fatalf("expected two db exchanges, got %d", len(exchanges))
	}
	request := exchanges[0]["request"].(map[string]any)
	if request["values"].([]any)[0] != "ACME" {
		t.Fatalf("statement values lost: %v", request)
	}
	rows := exchanges[0]["response"].(map[string]any)["rows"].([]any)
	if len(rows) != 1 {
		t.Fatalf("recorded rows lost: %v", exchanges[0]["response"])
	}
	failure := exchanges[1]["response"].(map[string]any)["error"].(map[string]any)
	if failure["message"] != "relation missing" || failure["code"] != "42P01" {
		t.Fatalf("recorded db error lost: %v", failure)
	}
}

func TestCaptureModeStampsTheDeterminismEnvelope(t *testing.T) {
	trace, err := Begin(&TraceContext{TraceID: "cap-x-4", CaptureEnvelope: true},
		"GET /quote", BeginOptions{})
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := trace.Events()[0]["at"].(json.Number); !ok {
		t.Fatalf("capture-mode event lacks a wall-clock stamp: %v", trace.Events()[0])
	}
	if _, ok := trace.Events()[0]["monoNs"].(json.Number); !ok {
		t.Fatalf("capture-mode event lacks a monotonic stamp: %v", trace.Events()[0])
	}
	// Scan-time traces must stay byte-stable: no envelope fields at all.
	scan, err := Begin(&TraceContext{TraceID: "trace-a"}, "GET /quote", BeginOptions{})
	if err != nil {
		t.Fatal(err)
	}
	if _, present := scan.Events()[0]["at"]; present {
		t.Fatalf("scan-time event gained an envelope stamp: %v", scan.Events()[0])
	}
	if _, present := scan.Events()[0]["monoNs"]; present {
		t.Fatalf("scan-time event gained an envelope stamp: %v", scan.Events()[0])
	}
}

func TestCapabilitiesClaimNetworkOnlyWithExchanges(t *testing.T) {
	plain := captureCapabilities(capturedOperation{events: []map[string]any{
		{"kind": "effect", "effect": "write"},
	}})
	for _, item := range plain {
		if item.(map[string]any)["capability"] == "network" {
			t.Fatal("network capability claimed without any recorded exchange")
		}
	}
	withExchange := captureCapabilities(capturedOperation{events: []map[string]any{
		{"kind": "effect", "exchange": map[string]any{"protocol": "http"}},
	}})
	found := false
	for _, item := range withExchange {
		if item.(map[string]any)["capability"] == "network" {
			found = true
		}
	}
	if !found {
		t.Fatal("network capability missing despite a recorded exchange")
	}
}

func TestResolveCommitPrefersConfigThenEnvironment(t *testing.T) {
	if got := resolveCommit("abc123"); got != "abc123" {
		t.Fatalf("configured commit ignored: %q", got)
	}
	t.Setenv("REPROIT_COMMIT", "env-commit")
	if got := resolveCommit(""); got != "env-commit" {
		t.Fatalf("REPROIT_COMMIT ignored: %q", got)
	}
	t.Setenv("REPROIT_COMMIT", "not a token")
	t.Setenv("GITHUB_SHA", "sha-fallback")
	if got := resolveCommit(""); got != "sha-fallback" {
		t.Fatalf("GITHUB_SHA fallback ignored: %q", got)
	}
}
