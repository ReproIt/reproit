// Child-process fixture for the reproitci tests: one upstream call, one
// assertion that fails unless FIXED=1. The upstream stub only boots outside
// replay, exactly like a real suite's dependencies.
package fixture

import (
	"encoding/json"
	"net"
	"net/http"
	"os"
	"testing"

	reproit "github.com/ReproIt/reproit/sdk/reproit-backend-go"
	"github.com/ReproIt/reproit/sdk/reproit-backend-go/reproitci"
)

const upstreamURL = "http://127.0.0.1:19995"

func TestMain(m *testing.M) {
	// os.Exit skips defers, so the upstream teardown lives in run.
	os.Exit(run(m))
}

func run(m *testing.M) int {
	if os.Getenv("REPROIT_REPLAY") == "" {
		listener, err := net.Listen("tcp", "127.0.0.1:19995")
		if err != nil {
			return 2
		}
		server := &http.Server{Handler: http.HandlerFunc(
			func(w http.ResponseWriter, r *http.Request) {
				w.Header().Set("Content-Type", "application/json")
				_, _ = w.Write([]byte(`{"n":7}`))
			})}
		go func() { _ = server.Serve(listener) }()
		defer func() { _ = server.Close() }()
	}
	return m.Run()
}

func TestAssertsTheUpstreamAnswer(t *testing.T) {
	ct := reproitci.Wrap(t, "unit")
	client := reproit.WrapClient(nil)
	request, err := http.NewRequestWithContext(
		ct.Context(), http.MethodGet, upstreamURL+"/n", nil)
	if err != nil {
		ct.Fatalf("build request: %v", err)
	}
	response, err := client.Do(request)
	if err != nil {
		ct.Fatalf("fetch: %v", err)
	}
	defer func() { _ = response.Body.Close() }()
	var body struct {
		N int `json:"n"`
	}
	if err := json.NewDecoder(response.Body).Decode(&body); err != nil {
		ct.Fatalf("decode: %v", err)
	}
	want := 8
	if os.Getenv("FIXED") == "1" {
		want = 7
	}
	if body.N != want {
		ct.Fatalf("upstream answered %d, want %d", body.N, want)
	}
}
