// Command hermeticfixture is the money-test fixture, Go flavor: a net/http
// app whose GET /quote operation 500s because an upstream pricing service
// returns {"prices": null} and the handler indexes into it.
//
// MODE=capture: boots the upstream, runs the failing operation through the
// instrument boundaries with a standalone trace, and writes a version-2
// `reproit-backend-capture` (exchanges + envelope) to CAPTURE_OUT.
// Default (server) mode: binds ONLY the app on $PORT; with REPROIT_REPLAY set
// the SDK serves the recorded exchanges, so no upstream and no database
// exist. FIXED=1 applies the fix.
package main

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"os"
	"strconv"
	"time"

	reproit "github.com/ReproIt/reproit/sdk/reproit-backend-go"
)

const upstreamAddr = "127.0.0.1:19981"

// pgLookup routes the statement through the boundary. Outside capture mode a
// live database would be reached, so it fails loudly: hermetic replay must
// serve the recorded rows before this closure ever runs.
func pgLookup(ctx context.Context, symbol string) error {
	_, err := reproit.RunDB(ctx,
		"SELECT id, symbol FROM issuers WHERE symbol = $1", []any{symbol},
		func() (reproit.DBOutcome, error) {
			if os.Getenv("MODE") != "capture" {
				return reproit.DBOutcome{}, &reproit.DBError{
					Message: "live database reached during hermetic replay",
				}
			}
			return reproit.DBOutcome{
				Command: "SELECT", RowCount: 1,
				Rows: []any{map[string]any{"id": 7, "symbol": symbol}},
			}, nil
		})
	return err
}

// quote is the planted operation: (status, output), matching the Node and
// Rust fixtures.
func quote(ctx context.Context, client *http.Client, upstream, symbol string) (int, any) {
	internal := map[string]any{"error": "internal"}
	if err := pgLookup(ctx, symbol); err != nil {
		return 500, internal
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodGet,
		upstream+"/prices?tier=gold", nil)
	if err != nil {
		return 500, internal
	}
	response, err := client.Do(request)
	if err != nil {
		return 500, internal
	}
	defer func() { _ = response.Body.Close() }()
	var body map[string]any
	if json.NewDecoder(response.Body).Decode(&body) != nil {
		return 500, internal
	}
	prices, isList := body["prices"].([]any)
	if os.Getenv("FIXED") == "1" && !isList {
		return 200, map[string]any{"first": nil, "note": "no prices available"}
	}
	if !isList || len(prices) == 0 {
		return 500, internal
	}
	return 200, map[string]any{"first": prices[0]}
}

func captureMode() error {
	upstream := &http.Server{
		Addr:              upstreamAddr,
		ReadHeaderTimeout: 5 * time.Second,
		Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{"prices":null}`))
		}),
	}
	listener, err := net.Listen("tcp", upstreamAddr)
	if err != nil {
		return err
	}
	go func() { _ = upstream.Serve(listener) }()
	defer func() { _ = upstream.Close() }()

	trace, err := reproit.Begin(&reproit.TraceContext{
		TraceID:         "cap-money-go-1",
		Build:           "money-fixture",
		CaptureEnvelope: true,
	}, "GET /quote", reproit.BeginOptions{
		Input: reproit.HTTPInput{Query: map[string]any{"symbol": "ACME"}}.Value(),
	})
	if err != nil {
		return err
	}
	ctx := reproit.ContextWithTrace(context.Background(), trace)
	status, output := quote(ctx, reproit.WrapClient(nil), "http://"+upstreamAddr, "ACME")
	if err := trace.Finish(output, status, status < 500, true); err != nil {
		return err
	}
	payload := map[string]any{
		"format":    "reproit-backend-capture",
		"version":   2,
		"operation": "GET /quote",
		"oracle":    "backend-server-error",
		"envelope": map[string]any{
			"observedAtMs": time.Now().UnixMilli(),
			"runtime":      "go",
			"os":           os.Getenv("GOOS"),
			"replaySeed":   "c0ffee00c0ffee00",
		},
		"events": trace.Events(),
	}
	out := os.Getenv("CAPTURE_OUT")
	if out == "" {
		return fmt.Errorf("CAPTURE_OUT is required in capture mode")
	}
	if err := os.WriteFile(out, reproit.CanonicalJSON(payload), 0o600); err != nil {
		return err
	}
	fmt.Println("capture fixture status", status)
	return nil
}

func serverMode() error {
	reproit.Init()
	client := reproit.WrapClient(nil)
	port := 19980
	if raw := os.Getenv("PORT"); raw != "" {
		if parsed, err := strconv.Atoi(raw); err == nil {
			port = parsed
		}
	}
	handler := http.NewServeMux()
	handler.HandleFunc("/quote", func(w http.ResponseWriter, r *http.Request) {
		symbol := r.URL.Query().Get("symbol")
		status, output := quote(r.Context(), client, "http://"+upstreamAddr, symbol)
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(status)
		_ = json.NewEncoder(w).Encode(output)
	})
	server := &http.Server{
		Addr:              "127.0.0.1:" + strconv.Itoa(port),
		Handler:           handler,
		ReadHeaderTimeout: 5 * time.Second,
	}
	fmt.Println("serving on", port)
	return server.ListenAndServe()
}

func main() {
	var err error
	if os.Getenv("MODE") == "capture" {
		err = captureMode()
	} else {
		err = serverMode()
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, "hermetic fixture:", err)
		os.Exit(1)
	}
}
