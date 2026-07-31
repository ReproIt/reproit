// Executes the shared behavioral vectors for the FROZEN runner wire, which is
// deliberately not the capture wire. This SDK is replay only: it never records
// a capture batch, so it has no inline body budget, no header table and no
// $reproit placeholder. Its whole shared surface with the rest of the fleet is
// the secret-key predicate, and eight languages hand implement that predicate.
// A divergence about which keys count as secret is silent in both directions:
// too narrow and a credential ships inside a capsule, too wide and a field
// replay needs is scrubbed into a placeholder that never matches.
// ../capture-behavior-v1.json states the predicate once so a defect is found
// once instead of eight times.
//
// One difference from the capture wire is deliberate and is asserted here so
// it cannot be closed by accident: idempotency_key IS secret on the capture
// wire and is NOT secret here. The runner list is thirteen parts, one shorter,
// because changing it would change bytes the fuzz harness compares.

package reproittui

import (
	"encoding/json"
	"os"
	"testing"
)

type causalVectors struct {
	Placeholder  string `json:"placeholder"`
	FoldingCases []struct {
		Field  string `json:"field"`
		Secret bool   `json:"secret"`
	} `json:"foldingCases"`
}

func loadCausalVectors(t *testing.T) causalVectors {
	t.Helper()
	raw, err := os.ReadFile("../capture-behavior-v1.json")
	if err != nil {
		t.Fatalf("read shared vectors: %v", err)
	}
	var doc struct {
		CausalRedaction causalVectors `json:"causalRedaction"`
	}
	if err := json.Unmarshal(raw, &doc); err != nil {
		t.Fatalf("parse shared vectors: %v", err)
	}
	if len(doc.CausalRedaction.FoldingCases) == 0 {
		t.Fatal("causalRedaction.foldingCases is empty")
	}
	return doc.CausalRedaction
}

func TestCausalRedactionFoldingCases(t *testing.T) {
	vectors := loadCausalVectors(t)
	for _, c := range vectors.FoldingCases {
		if secretKey(c.Field) != c.Secret {
			t.Fatalf("secretKey(%q) = %v, vector says %v", c.Field, !c.Secret, c.Secret)
		}
		safe := redactGo(map[string]interface{}{c.Field: "raw-value"}).(map[string]interface{})
		want := "raw-value"
		if c.Secret {
			want = "<reproit:string:length=9>"
		}
		if safe[c.Field] != want {
			t.Fatalf("redactGo %q = %v, want %q", c.Field, safe[c.Field], want)
		}
	}
}

func TestCausalRedactionPlaceholder(t *testing.T) {
	vectors := loadCausalVectors(t)
	for _, c := range vectors.FoldingCases {
		if !c.Secret {
			continue
		}
		safe := redactGo(map[string]interface{}{c.Field: 7}).(map[string]interface{})
		if safe[c.Field] != vectors.Placeholder {
			t.Fatalf("redactGo %q = %v, want %q", c.Field, safe[c.Field], vectors.Placeholder)
		}
	}
}
