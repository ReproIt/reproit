// Command contractsample emits the shared backend-SDK contract sample for
// sdk/test/backend_batch_test.js: one scan-time trace (for the header) and
// the 5xx capture batch a real Capture posts for the same failed operation,
// received through a local stub ingest server. Output: one JSON line with
// {"batch": ..., "header": ..., "headerName": "x-reproit-events"}.
package main

import (
	"encoding/json"
	"io"
	"net"
	"net/http"
	"os"
	"time"

	reproit "github.com/reproit/reproit-backend"
)

func main() {
	context := &reproit.TraceContext{TraceID: "trace-a"}
	trace, err := reproit.Begin(context, "createOrder", reproit.BeginOptions{
		Input: map[string]any{
			"item":     "widget",
			"password": "hunter22",
			"apiKey":   "sk_live_leak",
		},
	})
	if err != nil {
		fail(err)
	}
	err = trace.Effect(reproit.EffectWrite, reproit.EffectOptions{Resource: "orders", Key: "1"})
	if err != nil {
		fail(err)
	}
	if err := trace.Finish(map[string]any{"error": "boom"}, 500, false, true); err != nil {
		fail(err)
	}
	header, err := trace.Header()
	if err != nil {
		fail(err)
	}

	received := make(chan map[string]any, 1)
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		fail(err)
	}
	server := &http.Server{Handler: http.HandlerFunc(
		func(w http.ResponseWriter, r *http.Request) {
			body, _ := io.ReadAll(r.Body)
			var batch map[string]any
			if json.Unmarshal(body, &batch) == nil {
				select {
				case received <- batch:
				default:
				}
			}
			_, _ = w.Write([]byte(`{"accepted":true}`))
		})}
	go func() { _ = server.Serve(listener) }()
	defer server.Close()

	config := reproit.NewCaptureConfig(
		"http://"+listener.Addr().String()+"/v1/events", "sk", "app-demo")
	config.Build = "1.2.3"
	config.FlushInterval = 100 * time.Millisecond
	capture := reproit.NewCapture(config)
	if capture == nil {
		fail(nil)
	}
	capture.Record(trace)
	if !capture.Flush(5 * time.Second) {
		fail(nil)
	}
	batch := <-received

	encoded, err := json.Marshal(map[string]any{
		"batch":      batch,
		"header":     header,
		"headerName": "x-reproit-events",
	})
	if err != nil {
		fail(err)
	}
	os.Stdout.Write(append(encoded, '\n'))
}

func fail(err error) {
	if err != nil {
		os.Stderr.WriteString(err.Error() + "\n")
	} else {
		os.Stderr.WriteString("contract sample failed\n")
	}
	os.Exit(1)
}
