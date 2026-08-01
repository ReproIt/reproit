package reproitbackend

// Shared behavioral conformance vectors (sdk/capture-behavior-v1.json).
//
// Eleven SDKs hand implement one contract, so a defect otherwise has to be
// found eleven times. The groups below are harvested, not invented; each names
// the defect it pins:
//
//	bounds                    a budget measured in string length rather than
//	                          encoded bytes recorded 4096 characters of "€"
//	                          inline, 12288 bytes, past a budget the replayer
//	                          trusts.
//	headers                   Go's own instance: the 32 header cap applied
//	                          before sorting a randomized map recorded a
//	                          different subset each run. The cap is defined
//	                          over NAME SORTED order, so the generated case is
//	                          fed through http.Header's random iteration and
//	                          replayed 50 times.
//	redaction.typeCases       the $reproit stub must report the ORIGINAL type
//	                          and length, not "string" for everything.
//	redaction.foldingCases    secret detection folds case and separators and
//	                          matches substrings: "X-Authorization" and
//	                          "tokenizer" are secret, "username" is not.
//	redaction.nestingCases    redaction recurses through objects AND arrays; a
//	                          top-level-only scrub shipped nested keys in
//	                          plaintext.
//	redaction.structureCases  redaction preserves shape. No key dropped, no
//	                          array shortened, an explicit null stays a null
//	                          VALUE. An Android encoder dropping null map
//	                          values made a capsule say {"symbol":"ACME"}
//	                          where production sent {"prices":null}, and
//	                          replay reproduced a DIFFERENT error.
//	matching.cases            the replay matcher's contract: method and
//	                          path+query compared, host and scheme not, a
//	                          $reproit placeholder wildcards, nothing else
//	                          fuzzy. matching.pgCases pin the statement-text
//	                          matcher the same way.
//	divergence.cases          an unmatched call writes the REPROIT:DIVERGENCE
//	                          marker starting the stderr line, with the
//	                          required report fields.

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"reflect"
	"sort"
	"strings"
	"testing"
)

// One shape for every case in the document: the groups differ only in which
// fields they populate, and decoding them together keeps the loops uniform.
type behaviorCase struct {
	Name           string         `json:"name"`
	Input          map[string]any `json:"input"`
	InputGenerated map[string]any `json:"inputGenerated"`
	Expect         map[string]any `json:"expect"`
	Field          string         `json:"field"`
	Secret         bool           `json:"secret"`
}

type behaviorVectors struct {
	Constants struct {
		MaxExchangeBodyBytes int    `json:"maxExchangeBodyBytes"`
		MaxExchangeHeaders   int    `json:"maxExchangeHeaders"`
		DivergenceMarker     string `json:"divergenceMarker"`
	} `json:"constants"`
	Headers struct {
		Cases []behaviorCase `json:"cases"`
	} `json:"headers"`
	Bounds struct {
		Cases []behaviorCase `json:"cases"`
	} `json:"bounds"`
	Redaction struct {
		TypeCases      []behaviorCase `json:"typeCases"`
		FoldingCases   []behaviorCase `json:"foldingCases"`
		NestingCases   []behaviorCase `json:"nestingCases"`
		StructureCases []behaviorCase `json:"structureCases"`
	} `json:"redaction"`
	TriggerTokens struct {
		Allowed   []string          `json:"allowed"`
		Rejected  []string          `json:"rejected"`
		BySdkKind map[string]string `json:"bySdkKind"`
	} `json:"triggerTokens"`
	Matching struct {
		Cases   []matchVectorCase `json:"cases"`
		PgCases []matchVectorCase `json:"pgCases"`
	} `json:"matching"`
	Divergence struct {
		MarkerPrefix string `json:"markerPrefix"`
		ReportFields struct {
			Required []string `json:"required"`
		} `json:"reportFields"`
		Cases []divergenceVectorCase `json:"cases"`
	} `json:"divergence"`
}

type matchVectorCase struct {
	Name     string          `json:"name"`
	Recorded json.RawMessage `json:"recorded"`
	Live     json.RawMessage `json:"live"`
	Expect   struct {
		Matches bool `json:"matches"`
	} `json:"expect"`
}

type divergenceVectorCase struct {
	Name             string            `json:"name"`
	CapsuleExchanges []json.RawMessage `json:"capsuleExchanges"`
	Expect           struct {
		Diverged        bool            `json:"diverged"`
		Consumed        json.Number     `json:"consumed"`
		Total           json.Number     `json:"total"`
		ExpectedRequest json.RawMessage `json:"expectedRequest"`
	} `json:"expect"`
}

func loadVectors(t *testing.T) behaviorVectors {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join("..", "capture-behavior-v1.json"))
	if err != nil {
		t.Fatalf("read vectors: %v", err)
	}
	var vectors behaviorVectors
	// UseNumber so a vector's 42 stays an integer: the trace layer normalizes
	// to json.Number and metadata() reports "integer" vs "number" off it.
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	if err := decoder.Decode(&vectors); err != nil {
		t.Fatalf("parse vectors: %v", err)
	}
	return vectors
}

func TestBehaviorVectorConstants(t *testing.T) {
	vectors := loadVectors(t)
	if MaxExchangeBodyBytes != vectors.Constants.MaxExchangeBodyBytes {
		t.Fatalf("body bound %d, vectors say %d",
			MaxExchangeBodyBytes, vectors.Constants.MaxExchangeBodyBytes)
	}
	if maxExchangeHeaders != vectors.Constants.MaxExchangeHeaders {
		t.Fatalf("header cap %d, vectors say %d",
			maxExchangeHeaders, vectors.Constants.MaxExchangeHeaders)
	}
	if DivergenceMarker != vectors.Constants.DivergenceMarker {
		t.Fatalf("marker %q, vectors say %q", DivergenceMarker, vectors.Constants.DivergenceMarker)
	}
}

// vectorBody renders `body` verbatim or expands `bodyRepeat: [unit, count]`.
// The euro case only bites when the budget counts ENCODED BYTES.
func vectorBody(spec map[string]any) []byte {
	if repeat, ok := spec["bodyRepeat"].([]any); ok {
		return []byte(strings.Repeat(repeat[0].(string), vectorInt(repeat[1])))
	}
	if body, ok := spec["body"].(string); ok {
		return []byte(body)
	}
	return nil
}

func vectorInt(value any) int {
	number, ok := value.(json.Number)
	if !ok {
		return 0
	}
	parsed, err := number.Int64()
	if err != nil {
		return 0
	}
	return int(parsed)
}

func TestBehaviorVectorBounds(t *testing.T) {
	vectors := loadVectors(t)
	for _, kase := range vectors.Bounds.Cases {
		contentType, _ := kase.Input["contentType"].(string)
		actual := boundedBody(vectorBody(kase.Input), contentType)
		if actual == nil {
			actual = map[string]any{}
		}
		expect := map[string]any{}
		for key, value := range kase.Expect {
			expect[key] = value
		}
		if body, ok := expect["body"].(map[string]any); ok {
			if repeat, ok := body["repeat"].([]any); ok {
				expect["body"] = strings.Repeat(repeat[0].(string), vectorInt(repeat[1]))
			}
		}
		if !reflect.DeepEqual(actual, expect) {
			t.Fatalf("bounds case %s: got %v, want %v", kase.Name, actual, expect)
		}
	}
}

func TestBehaviorVectorHeaders(t *testing.T) {
	vectors := loadVectors(t)
	for _, kase := range vectors.Headers.Cases {
		if kase.InputGenerated == nil {
			literalHeaderCase(t, kase)
			continue
		}
		generatedHeaderCase(t, kase)
	}
}

func literalHeaderCase(t *testing.T, kase behaviorCase) {
	t.Helper()
	headers := http.Header{}
	given, _ := kase.Input["headers"].(map[string]any)
	for name, value := range given {
		headers[name] = []string{value.(string)}
	}
	actual := boundedHeaders(headers)
	if actual == nil {
		actual = map[string]any{}
	}
	if !reflect.DeepEqual(actual, kase.Expect) {
		t.Fatalf("headers case %s: got %v, want %v", kase.Name, actual, kase.Expect)
	}
}

// The Go defect verbatim: the cap was applied before sorting, so a randomized
// map iteration recorded a different subset each run. The same input must
// produce byte identical output, and that output must be the sorted prefix.
func generatedHeaderCase(t *testing.T, kase behaviorCase) {
	t.Helper()
	pattern, _ := kase.InputGenerated["namePattern"].(string)
	value, _ := kase.InputGenerated["value"].(string)
	headers := http.Header{}
	for index := 0; index < vectorInt(kase.InputGenerated["headerCount"]); index++ {
		headers[fmt.Sprintf(pattern, index)] = []string{value}
	}

	first := ""
	for run := 0; run < 50; run++ {
		encoded, err := json.Marshal(boundedHeaders(headers))
		if err != nil {
			t.Fatalf("marshal: %v", err)
		}
		if run == 0 {
			first = string(encoded)
			continue
		}
		if string(encoded) != first {
			t.Fatalf("headers case %s: subset varies across runs; run %d differs from run 0\n"+
				"first: %s\nnow:   %s", kase.Name, run, first, encoded)
		}
	}

	var decoded map[string]map[string]string
	if err := json.Unmarshal([]byte(first), &decoded); err != nil {
		t.Fatalf("decode: %v", err)
	}
	names := make([]string, 0, len(decoded["headers"]))
	for name := range decoded["headers"] {
		names = append(names, name)
	}
	sort.Strings(names)
	if len(names) != vectorInt(kase.Expect["headerCount"]) {
		t.Fatalf("headers case %s: kept %d headers, vectors say %d",
			kase.Name, len(names), vectorInt(kase.Expect["headerCount"]))
	}
	// The cap is over sorted names, not the order the headers arrived in.
	if names[0] != kase.Expect["firstName"] {
		t.Fatalf("headers case %s: first kept name %q, vectors say %v",
			kase.Name, names[0], kase.Expect["firstName"])
	}
	if names[len(names)-1] != kase.Expect["lastName"] {
		t.Fatalf("headers case %s: last kept name %q, vectors say %v",
			kase.Name, names[len(names)-1], kase.Expect["lastName"])
	}
}

func TestBehaviorVectorRedactionTypes(t *testing.T) {
	vectors := loadVectors(t)
	for _, kase := range vectors.Redaction.TypeCases {
		assertRedaction(t, "type", kase)
	}
}

func TestBehaviorVectorRedactionNesting(t *testing.T) {
	vectors := loadVectors(t)
	for _, kase := range vectors.Redaction.NestingCases {
		assertRedaction(t, "nesting", kase)
	}
}

// Structure preservation: a dropped key, a shortened array or a collapsed null
// changes the shape the replay matcher walks, and the capsule stops matching
// the live call it was recorded from.
func TestBehaviorVectorRedactionStructure(t *testing.T) {
	vectors := loadVectors(t)
	for _, kase := range vectors.Redaction.StructureCases {
		assertRedaction(t, "structure", kase)
	}
}

func assertRedaction(t *testing.T, group string, kase behaviorCase) {
	t.Helper()
	actual := redact(normalize(kase.Input))
	expect := normalize(map[string]any(kase.Expect))
	// Only the structure group names its cases; the rest are identified by
	// the input they redact.
	label := kase.Name
	if label == "" {
		label = fmt.Sprintf("%v", kase.Input)
	}
	if !reflect.DeepEqual(actual, expect) {
		t.Fatalf("%s case %s: got %v, want %v", group, label, actual, expect)
	}
}

func TestBehaviorVectorRedactionFolding(t *testing.T) {
	vectors := loadVectors(t)
	for _, kase := range vectors.Redaction.FoldingCases {
		out, ok := redact(map[string]any{kase.Field: "value"}).(map[string]any)
		if !ok {
			t.Fatalf("folding case %s: redact did not return an object", kase.Field)
		}
		stub, _ := out[kase.Field].(map[string]any)
		_, wasRedacted := stub["$reproit"]
		if wasRedacted != kase.Secret {
			t.Fatalf("folding case %s: redacted=%v, vectors say secret=%v",
				kase.Field, wasRedacted, kase.Secret)
		}
	}
}

func orderedVector(t *testing.T, raw json.RawMessage) any {
	t.Helper()
	if len(raw) == 0 {
		return nil
	}
	decoded, err := decodeOrderedJSON(raw)
	if err != nil {
		t.Fatalf("decode vector value: %v", err)
	}
	return decoded
}

func TestBehaviorVectorMatching(t *testing.T) {
	vectors := loadVectors(t)
	if len(vectors.Matching.Cases) == 0 {
		t.Fatal("matching cases missing from the vectors")
	}
	for _, kase := range vectors.Matching.Cases {
		actual := httpRequestMatches(
			orderedVector(t, kase.Recorded), orderedVector(t, kase.Live))
		if actual != kase.Expect.Matches {
			t.Fatalf("matching case %s: got %v, want %v",
				kase.Name, actual, kase.Expect.Matches)
		}
	}
}

func TestBehaviorVectorPgMatching(t *testing.T) {
	vectors := loadVectors(t)
	if len(vectors.Matching.PgCases) == 0 {
		t.Fatal("pg matching cases missing from the vectors")
	}
	for _, kase := range vectors.Matching.PgCases {
		actual := dbRequestMatches(
			orderedVector(t, kase.Recorded), orderedVector(t, kase.Live))
		if actual != kase.Expect.Matches {
			t.Fatalf("pg matching case %s: got %v, want %v",
				kase.Name, actual, kase.Expect.Matches)
		}
	}
}

func TestBehaviorVectorDivergenceMarker(t *testing.T) {
	vectors := loadVectors(t)
	if len(vectors.Divergence.Cases) == 0 {
		t.Fatal("divergence cases missing from the vectors")
	}
	kase := vectors.Divergence.Cases[0]
	events := make([]string, 0, len(kase.CapsuleExchanges))
	for index, exchange := range kase.CapsuleExchanges {
		events = append(events, fmt.Sprintf(
			`{"kind":"effect","sequence":%d,"exchange":%s}`, index+1, exchange))
	}
	payload := `{"format":"reproit-backend-capture","version":2,` +
		`"operation":"GET /x","oracle":"backend-server-error",` +
		`"events":[` + strings.Join(events, ",") + `]}`
	loaded := loadedCapture(t, payload)
	reader, writer, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	stderr := os.Stderr
	os.Stderr = writer
	hit := loaded.matched("http", omap{
		{"method", "GET"}, {"url", "http://svc/unknown"},
	})
	os.Stderr = stderr
	_ = writer.Close()
	if hit != nil {
		t.Fatal("an unmatched probe matched")
	}
	emitted, _ := io.ReadAll(reader)
	line := strings.TrimSuffix(string(emitted), "\n")
	prefix := vectors.Divergence.MarkerPrefix
	// The prefix must START the line: Ruby's warn prefix broke the CLI match.
	if !strings.HasPrefix(line, prefix) {
		t.Fatalf("marker does not start the stderr line: %q", line)
	}
	var report map[string]any
	if json.Unmarshal([]byte(line[len(prefix):]), &report) != nil {
		t.Fatalf("divergence report is not JSON: %q", line)
	}
	for _, field := range vectors.Divergence.ReportFields.Required {
		if _, present := report[field]; !present {
			t.Fatalf("report lacks required field %q: %v", field, report)
		}
	}
	if fmt.Sprintf("%v", report["consumed"]) != kase.Expect.Consumed.String() {
		t.Fatalf("consumed %v, vectors say %s", report["consumed"], kase.Expect.Consumed)
	}
	if fmt.Sprintf("%v", report["total"]) != kase.Expect.Total.String() {
		t.Fatalf("total %v, vectors say %s", report["total"], kase.Expect.Total)
	}
	var expectedRequest any
	if err := json.Unmarshal(kase.Expect.ExpectedRequest, &expectedRequest); err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(report["expected"], expectedRequest) {
		t.Fatalf("expected request %v, vectors say %v",
			report["expected"], expectedRequest)
	}
}

func TestBehaviorVectorTriggerToken(t *testing.T) {
	vectors := loadVectors(t)
	token := vectors.TriggerTokens.BySdkKind["backend"]
	allowed := false
	for _, candidate := range vectors.TriggerTokens.Allowed {
		if candidate == token {
			allowed = true
		}
	}
	if !allowed {
		t.Fatalf("backend trigger token %q is not in the protocol vocabulary", token)
	}
	source, err := os.ReadFile("capture.go")
	if err != nil {
		t.Fatalf("read capture.go: %v", err)
	}
	if !strings.Contains(string(source), `"`+token+`"`) {
		t.Fatalf("capture.go must emit %q", token)
	}
	for _, bad := range vectors.TriggerTokens.Rejected {
		if strings.Contains(string(source), `"`+bad+`"`) {
			t.Fatalf("capture.go must not emit %q; iOS and RN both shipped user-action", bad)
		}
	}
}
