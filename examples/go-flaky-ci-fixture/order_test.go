// Planted order-dependent test failure that fires only under CI-like
// conditions, for the flaky-CI wedge (Track 3).
//
// The first test runs ONLY on the CI legacy matrix (CI_LEGACY_MATRIX=1) and
// leaks state into the shared config service: it switches the service to its
// legacy response format, which returns the tax rate as a string. The second
// test then computes a wrong total and fails. A plain local run never takes
// the legacy branch, so the suite passes and the failure looks
// unreproducible ("flaky"). The capsule spooled by the CI run carries the
// recorded legacy response, so `reproit check <capsule> --exec "cd <dir> &&
// go test -count=1 -run '^TestOrderTotalAppliesTheConfiguredTaxRate$' 1>&2"`
// re-executes the exact failing run anywhere. The `1>&2` matters: go test
// merges the test binary's stderr into stdout, and `reproit check` reads the
// markers from stderr.
package order

import (
	"net"
	"net/http"
	"os"
	"sync/atomic"
	"testing"

	reproit "github.com/ReproIt/reproit/sdk/reproit-backend-go"
	"github.com/ReproIt/reproit/sdk/reproit-backend-go/reproitci"
)

const configURL = "http://127.0.0.1:19992"

func TestMain(m *testing.M) {
	// os.Exit skips defers, so the config-service teardown lives in run.
	os.Exit(run(m))
}

// run boots the shared config service both tests talk to. Stateful on
// purpose: the legacy-format test leaks its toggle into it. Never started
// under replay, where the SDK serves the recorded exchanges in process and
// any real socket attempt would surface as a divergence, not a connection.
func run(m *testing.M) int {
	if os.Getenv("REPROIT_REPLAY") == "" {
		var legacy atomic.Bool
		listener, err := net.Listen("tcp", "127.0.0.1:19992")
		if err != nil {
			return 2
		}
		server := &http.Server{Handler: http.HandlerFunc(
			func(w http.ResponseWriter, r *http.Request) {
				if r.Method == http.MethodPost && r.URL.Path == "/format/legacy" {
					legacy.Store(true)
					w.WriteHeader(http.StatusNoContent)
					return
				}
				w.Header().Set("Content-Type", "application/json")
				if legacy.Load() {
					_, _ = w.Write([]byte(`{"rate":"0.25"}`))
					return
				}
				_, _ = w.Write([]byte(`{"rate":0.25}`))
			})}
		go func() { _ = server.Serve(listener) }()
		defer func() { _ = server.Close() }()
	}
	return m.Run()
}

func TestLegacyConfigFormatToggles(t *testing.T) {
	ct := reproitci.Wrap(t, "checkout")
	// CI-only: this is the state leak that makes the next test order
	// dependent. A local run never takes this branch.
	if os.Getenv("CI_LEGACY_MATRIX") != "1" {
		return
	}
	request, err := http.NewRequestWithContext(
		ct.Context(), http.MethodPost, configURL+"/format/legacy", nil)
	if err != nil {
		ct.Fatalf("build request: %v", err)
	}
	response, err := reproit.WrapClient(nil).Do(request)
	if err != nil {
		ct.Fatalf("toggle legacy format: %v", err)
	}
	_ = response.Body.Close()
	if response.StatusCode != http.StatusNoContent {
		ct.Fatalf("toggle answered %d, want 204", response.StatusCode)
	}
}

func TestOrderTotalAppliesTheConfiguredTaxRate(t *testing.T) {
	ct := reproitci.Wrap(t, "checkout")
	total, err := OrderTotal(ct.Context(), reproit.WrapClient(nil), configURL, 100)
	if err != nil {
		ct.Fatalf("order total: %v", err)
	}
	if total != 125 {
		ct.Fatalf("order total = %v, want 125", total)
	}
}
