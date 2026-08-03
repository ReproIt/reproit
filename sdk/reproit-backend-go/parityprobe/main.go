// Command parityprobe is the Go side of sdk/test/backend_replay_parity_test.js:
// it loads the capsule from stdin as a REPROIT_REPLAY session and replays the
// harness's two probes through the real Transport boundary, printing the
// served SSE exchange (status, body, observed chunk split), the 599
// divergence body, and the captured REPROIT:DIVERGENCE marker line as one
// JSON object on stdout. The harness byte-compares all three against the
// Node reference.
package main

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"

	reproit "github.com/ReproIt/reproit/sdk/reproit-backend-go"
)

func fail(context string, err error) {
	fmt.Fprintln(os.Stderr, "parityprobe:", context+":", err)
	os.Exit(1)
}

func main() {
	capsule, err := io.ReadAll(os.Stdin)
	if err != nil {
		fail("read capsule", err)
	}
	dir, err := os.MkdirTemp("", "reproit-parity")
	if err != nil {
		fail("temp dir", err)
	}
	defer func() { _ = os.RemoveAll(dir) }()
	path := filepath.Join(dir, "capture.json")
	if err := os.WriteFile(path, capsule, 0o600); err != nil {
		fail("write capsule", err)
	}
	if err := os.Setenv("REPROIT_REPLAY", path); err != nil {
		fail("set env", err)
	}
	reproit.Init()
	if !reproit.Replaying() {
		fail("session", fmt.Errorf("capsule did not load as a replay session"))
	}
	transport := &reproit.Transport{}

	// Probe 1: the recorded SSE exchange, chunk boundaries observed as the
	// body is consumed (one Read per recorded chunk).
	request, err := http.NewRequest(http.MethodGet, "http://llm.internal/stream", nil)
	if err != nil {
		fail("build stream request", err)
	}
	response, err := transport.RoundTrip(request)
	if err != nil {
		fail("serve stream", err)
	}
	chunks := []string{}
	buffer := make([]byte, 64*1024)
	for {
		read, readErr := response.Body.Read(buffer)
		if read > 0 {
			chunks = append(chunks, string(buffer[:read]))
		}
		if readErr != nil {
			break
		}
	}
	_ = response.Body.Close()

	// Probe 2: prompt drift. The marker goes to stderr; capture it through a
	// pipe so the harness reads it from stdout alongside the served body.
	reader, writer, err := os.Pipe()
	if err != nil {
		fail("stderr pipe", err)
	}
	stderr := os.Stderr
	os.Stderr = writer
	driftBody := `{"messages":[{"role":"user","content":"hello"},` +
		`{"role":"assistant","content":"hi"},` +
		`{"role":"user","content":"DIFFERENT QUESTION"}]}`
	drift, err := http.NewRequest(http.MethodPost, "http://llm.internal/v1/chat",
		strings.NewReader(driftBody))
	if err != nil {
		os.Stderr = stderr
		fail("build drift request", err)
	}
	drift.Header.Set("Content-Type", "application/json")
	diverged, err := transport.RoundTrip(drift)
	os.Stderr = stderr
	_ = writer.Close()
	if err != nil {
		fail("serve drift", err)
	}
	divergedBody, err := io.ReadAll(diverged.Body)
	if err != nil {
		fail("read diverged body", err)
	}
	emitted, err := io.ReadAll(reader)
	if err != nil {
		fail("read marker", err)
	}
	marker := ""
	for _, line := range strings.Split(string(emitted), "\n") {
		if strings.HasPrefix(line, reproit.DivergenceMarker) {
			marker = line
			break
		}
	}
	if marker == "" {
		fail("marker", fmt.Errorf("no divergence marker on stderr: %q", emitted))
	}

	report := map[string]any{
		"serve": map[string]any{
			"status":   response.StatusCode,
			"bodyText": strings.Join(chunks, ""),
			"chunks":   chunks,
		},
		"divergedBody": string(divergedBody),
		"marker":       marker,
	}
	encoded, err := json.Marshal(report)
	if err != nil {
		fail("encode report", err)
	}
	fmt.Println(string(encoded))
}
