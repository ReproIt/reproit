package reproitbackend

// Shared behavioral conformance vectors (sdk/capture-behavior-v1.json).
//
// Eleven SDKs hand implement one contract, so a defect otherwise has to be
// found eleven times. Go's own instance was the 32 header cap applied before
// sorting a randomized map, which recorded a different subset each run. The
// headers group below pins exactly that.

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"testing"
)

type behaviorVectors struct {
	Constants struct {
		MaxExchangeBodyBytes int    `json:"maxExchangeBodyBytes"`
		MaxExchangeHeaders   int    `json:"maxExchangeHeaders"`
		DivergenceMarker     string `json:"divergenceMarker"`
	} `json:"constants"`
	Headers struct {
		Cases []struct {
			Name             string            `json:"name"`
			Input            map[string]any    `json:"input"`
			InputGenerated   map[string]any    `json:"inputGenerated"`
			Expect           map[string]any    `json:"expect"`
		} `json:"cases"`
	} `json:"headers"`
	Bounds struct {
		Cases []struct {
			Name   string         `json:"name"`
			Input  map[string]any `json:"input"`
			Expect map[string]any `json:"expect"`
		} `json:"cases"`
	} `json:"bounds"`
	TriggerTokens struct {
		Allowed  []string          `json:"allowed"`
		Rejected []string          `json:"rejected"`
		BySdkKind map[string]string `json:"bySdkKind"`
	} `json:"triggerTokens"`
}

func loadVectors(t *testing.T) behaviorVectors {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join("..", "capture-behavior-v1.json"))
	if err != nil {
		t.Fatalf("read vectors: %v", err)
	}
	var vectors behaviorVectors
	if err := json.Unmarshal(raw, &vectors); err != nil {
		t.Fatalf("parse vectors: %v", err)
	}
	return vectors
}

func TestBehaviorVectorConstants(t *testing.T) {
	vectors := loadVectors(t)
	if MaxExchangeBodyBytes != vectors.Constants.MaxExchangeBodyBytes {
		t.Fatalf("body bound %d, vectors say %d", MaxExchangeBodyBytes, vectors.Constants.MaxExchangeBodyBytes)
	}
	if maxExchangeHeaders != vectors.Constants.MaxExchangeHeaders {
		t.Fatalf("header cap %d, vectors say %d", maxExchangeHeaders, vectors.Constants.MaxExchangeHeaders)
	}
	if DivergenceMarker != vectors.Constants.DivergenceMarker {
		t.Fatalf("marker %q, vectors say %q", DivergenceMarker, vectors.Constants.DivergenceMarker)
	}
}

// The Go defect: the cap was applied before sorting, so a randomized map
// iteration recorded a different subset each run. Running the same input many
// times must produce byte identical output.
func TestBehaviorVectorHeaderCapIsDeterministic(t *testing.T) {
	vectors := loadVectors(t)
	headers := http.Header{}
	for i := 0; i < 40; i++ {
		headers.Set(fmt.Sprintf("x-h%02d", i), "v")
	}

	first := ""
	for run := 0; run < 50; run++ {
		bounded := boundedHeaders(headers)
		encoded, err := json.Marshal(bounded)
		if err != nil {
			t.Fatalf("marshal: %v", err)
		}
		if run == 0 {
			first = string(encoded)
			continue
		}
		if string(encoded) != first {
			t.Fatalf("header subset varies across runs; run %d differs from run 0\nfirst: %s\nnow:   %s",
				run, first, encoded)
		}
	}

	var decoded map[string]map[string]string
	if err := json.Unmarshal([]byte(first), &decoded); err != nil {
		t.Fatalf("decode: %v", err)
	}
	got := decoded["headers"]
	if len(got) != vectors.Constants.MaxExchangeHeaders {
		t.Fatalf("kept %d headers, cap is %d", len(got), vectors.Constants.MaxExchangeHeaders)
	}
	if _, ok := got["x-h00"]; !ok {
		t.Fatalf("sorted cap must keep x-h00; got %v", got)
	}
	if _, ok := got["x-h31"]; !ok {
		t.Fatalf("sorted cap must keep x-h31; got %v", got)
	}
	if _, ok := got["x-h32"]; ok {
		t.Fatalf("sorted cap must drop x-h32; got %v", got)
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
	if !containsToken(string(source), token) {
		t.Fatalf("capture.go must emit %q", token)
	}
	for _, bad := range vectors.TriggerTokens.Rejected {
		if containsToken(string(source), bad) {
			t.Fatalf("capture.go must not emit %q; iOS and RN both shipped user-action", bad)
		}
	}
}

func containsToken(source, token string) bool {
	return len(token) > 0 && (contains(source, `"`+token+`"`))
}

func contains(haystack, needle string) bool {
	for i := 0; i+len(needle) <= len(haystack); i++ {
		if haystack[i:i+len(needle)] == needle {
			return true
		}
	}
	return false
}
