// CI capture mode: a failing test spools a test-trigger capsule, a replay
// run re-executes only the named test and reports the structured result
// marker, and the spool cap drops loudly. Each scenario runs the wrapped
// fixture suite in a child `go test` process because capture/replay mode is
// decided by env at Wrap time and the replay session pins process state.
package reproitci

import (
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
)

// runFixture executes the testdata fixture suite with the given env. go test
// merges the test binary's stderr into stdout, so markers are asserted on
// the combined output.
func runFixture(t *testing.T, env map[string]string) (string, error) {
	t.Helper()
	dir, err := filepath.Abs(filepath.Join("testdata", "fixture"))
	if err != nil {
		t.Fatal(err)
	}
	// Local directory mode (no package argument): package-list mode buffers
	// a PASSING binary's output away, which would hide the result marker.
	cmd := exec.Command("go", "test", "-count=1")
	cmd.Dir = dir
	cmd.Env = os.Environ()
	for name, value := range env {
		cmd.Env = append(cmd.Env, name+"="+value)
	}
	output, runErr := cmd.CombinedOutput()
	return string(output), runErr
}

func capsuleIn(t *testing.T, spool string) string {
	t.Helper()
	matches, err := filepath.Glob(filepath.Join(spool, "capsule-*.json"))
	if err != nil {
		t.Fatal(err)
	}
	if len(matches) != 1 {
		t.Fatalf("expected 1 capsule, got %d", len(matches))
	}
	return matches[0]
}

const fixtureOperation = "test:unit#TestAssertsTheUpstreamAnswer"

func TestFailingTestSpoolsATestTriggerCapsuleWithTheExchange(t *testing.T) {
	spool := t.TempDir()
	output, err := runFixture(t, map[string]string{
		"REPROIT_CI_CAPTURE": "1",
		"REPROIT_CI_SPOOL":   spool,
	})
	if err == nil {
		t.Fatalf("the failing suite exited 0:\n%s", output)
	}
	if !strings.Contains(output, SpoolMarker) {
		t.Fatalf("spool marker missing:\n%s", output)
	}
	raw, err := os.ReadFile(capsuleIn(t, spool))
	if err != nil {
		t.Fatal(err)
	}
	var capsule struct {
		Format    string           `json:"format"`
		Version   int              `json:"version"`
		Operation string           `json:"operation"`
		Oracle    string           `json:"oracle"`
		Envelope  map[string]any   `json:"envelope"`
		Events    []map[string]any `json:"events"`
	}
	if err := json.Unmarshal(raw, &capsule); err != nil {
		t.Fatal(err)
	}
	if capsule.Format != "reproit-backend-capture" || capsule.Version != 2 {
		t.Fatalf("wrong capsule identity: %s v%d", capsule.Format, capsule.Version)
	}
	if capsule.Operation != fixtureOperation {
		t.Fatalf("operation %q", capsule.Operation)
	}
	if capsule.Oracle != TestFailureOracle {
		t.Fatalf("oracle %q", capsule.Oracle)
	}
	if seed, _ := capsule.Envelope["replaySeed"].(string); seed == "" {
		t.Fatalf("envelope lacks a replay seed: %v", capsule.Envelope)
	}
	exchanges := 0
	for _, event := range capsule.Events {
		exchange, ok := event["exchange"].(map[string]any)
		if !ok {
			continue
		}
		exchanges++
		response := exchange["response"].(map[string]any)
		body := response["body"].(map[string]any)
		if body["n"] != float64(7) {
			t.Fatalf("recorded upstream answer wrong: %v", body)
		}
	}
	if exchanges != 1 {
		t.Fatalf("expected 1 recorded exchange, got %d", exchanges)
	}
	returned := capsule.Events[len(capsule.Events)-1]
	if returned["success"] != false {
		t.Fatalf("return event not failed: %v", returned)
	}
	failure, _ := returned["output"].(map[string]any)["error"].(string)
	if !strings.Contains(failure, "upstream answered 7, want 8") {
		t.Fatalf("recorded failure identity wrong: %q", failure)
	}
}

func TestReplayReRunsTheNamedTestAndReportsFailedThenPassed(t *testing.T) {
	spool := t.TempDir()
	output, err := runFixture(t, map[string]string{
		"REPROIT_CI_CAPTURE": "1",
		"REPROIT_CI_SPOOL":   spool,
	})
	if err == nil {
		t.Fatalf("the capture run exited 0:\n%s", output)
	}
	capsule := capsuleIn(t, spool)
	// No upstream exists in either replay run; the SDK serves the recording.
	failed, err := runFixture(t, map[string]string{"REPROIT_REPLAY": capsule})
	if err == nil {
		t.Fatalf("the unfixed replay exited 0:\n%s", failed)
	}
	line := ""
	for _, candidate := range strings.Split(failed, "\n") {
		if strings.HasPrefix(candidate, ResultMarker) {
			line = candidate
			break
		}
	}
	if line == "" {
		t.Fatalf("result marker missing:\n%s", failed)
	}
	var report struct {
		Operation string `json:"operation"`
		Status    string `json:"status"`
		Failure   string `json:"failure"`
	}
	if err := json.Unmarshal([]byte(line[len(ResultMarker):]), &report); err != nil {
		t.Fatal(err)
	}
	if report.Status != "failed" || report.Operation != fixtureOperation {
		t.Fatalf("wrong replay report: %+v", report)
	}
	if !strings.Contains(report.Failure, "upstream answered 7, want 8") {
		t.Fatalf("replay failure identity wrong: %q", report.Failure)
	}
	passed, err := runFixture(t, map[string]string{
		"REPROIT_REPLAY": capsule,
		"FIXED":          "1",
	})
	if err != nil {
		t.Fatalf("the fixed replay failed: %v\n%s", err, passed)
	}
	if !strings.Contains(passed, `"status":"passed"`) {
		t.Fatalf("passed marker missing:\n%s", passed)
	}
}

func TestAFullSpoolDropsTheCapsuleAndCountsTheDrop(t *testing.T) {
	spool := t.TempDir()
	// Pre-fill the spool to the floor cap so the next capsule cannot fit.
	filler := strings.Repeat("x", spoolMaxFloorBytes)
	err := os.WriteFile(filepath.Join(spool, "existing.json"), []byte(filler), 0o600)
	if err != nil {
		t.Fatal(err)
	}
	output, runErr := runFixture(t, map[string]string{
		"REPROIT_CI_CAPTURE":   "1",
		"REPROIT_CI_SPOOL":     spool,
		"REPROIT_CI_SPOOL_MAX": strconv.Itoa(spoolMaxFloorBytes),
	})
	if runErr == nil {
		t.Fatalf("the failing suite exited 0:\n%s", output)
	}
	matches, err := filepath.Glob(filepath.Join(spool, "capsule-*.json"))
	if err != nil {
		t.Fatal(err)
	}
	if len(matches) != 0 {
		t.Fatalf("a capsule was spooled past the cap: %v", matches)
	}
	raw, err := os.ReadFile(filepath.Join(spool, "dropped.count"))
	if err != nil {
		t.Fatal(err)
	}
	if strings.TrimSpace(string(raw)) != "1" {
		t.Fatalf("dropped.count = %q, want 1", strings.TrimSpace(string(raw)))
	}
}

func TestWithoutCaptureOrReplayEnvTheWrapperIsInert(t *testing.T) {
	output, err := runFixture(t, nil)
	if err == nil {
		t.Fatalf("the unfixed suite exited 0:\n%s", output)
	}
	if strings.Contains(output, SpoolMarker) || strings.Contains(output, ResultMarker) {
		t.Fatalf("inert mode emitted markers:\n%s", output)
	}
}

func TestOperationNamesAreBounded(t *testing.T) {
	long := strings.Repeat("s", 400)
	operation := operationFor(" "+long+" ", "TestX")
	if operation != TestTriggerPrefix+strings.Repeat("s", maxName)+"#TestX" {
		t.Fatalf("suite bound not applied: %q", operation)
	}
}

func TestReplayTargetRejectsNonTestCapsules(t *testing.T) {
	path := filepath.Join(t.TempDir(), "http.json")
	payload := `{"format":"reproit-backend-capture","operation":"GET /quote"}`
	if err := os.WriteFile(path, []byte(payload), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := replayTarget(path); err == nil {
		t.Fatal("a request-trigger capsule was accepted as a replay target")
	}
	test := filepath.Join(t.TempDir(), "test.json")
	payload = `{"format":"reproit-backend-capture","operation":"test:unit#TestX"}`
	if err := os.WriteFile(test, []byte(payload), 0o600); err != nil {
		t.Fatal(err)
	}
	target, err := replayTarget(test)
	if err != nil || target != "test:unit#TestX" {
		t.Fatalf("target %q err %v", target, err)
	}
}

func TestSpoolMaxIsClampedToItsBounds(t *testing.T) {
	t.Setenv("REPROIT_CI_SPOOL_MAX", "1")
	if spoolMaxBytes() != spoolMaxFloorBytes {
		t.Fatalf("floor not applied: %d", spoolMaxBytes())
	}
	t.Setenv("REPROIT_CI_SPOOL_MAX", strconv.Itoa(1 << 30))
	if spoolMaxBytes() != spoolMaxCeilBytes {
		t.Fatalf("ceiling not applied: %d", spoolMaxBytes())
	}
	t.Setenv("REPROIT_CI_SPOOL_MAX", "not-a-number")
	if spoolMaxBytes() != DefaultSpoolMaxBytes {
		t.Fatalf("default not applied: %d", spoolMaxBytes())
	}
}
