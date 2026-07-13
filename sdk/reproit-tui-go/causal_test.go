package reproittui

import (
	"encoding/json"
	"strings"
	"testing"
)

func TestExplicitSecretKeysRedactWithoutHidingOrdinaryKeys(t *testing.T) {
	safe := redactGo(map[string]interface{}{
		"apiKey": "raw-api", "publishable-key": "raw-pub", "private_key": "raw-private",
		"access.key": "raw-access", "signing key": "raw-signing",
		"keyboardLayout": "dvorak", "key": "ordinary",
	}).(map[string]interface{})
	if safe["keyboardLayout"] != "dvorak" || safe["key"] != "ordinary" {
		t.Fatalf("harmless keys were redacted: %#v", safe)
	}
	encoded, _ := json.Marshal(safe)
	for _, raw := range []string{"raw-api", "raw-pub", "raw-private", "raw-access", "raw-signing"} {
		if strings.Contains(string(encoded), raw) {
			t.Fatalf("raw secret survived: %s", raw)
		}
	}
}
