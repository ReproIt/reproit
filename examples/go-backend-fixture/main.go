// Money-test fixture for Go capsule parity: a net/http app with the reproit
// SDK whose GET /quote operation 500s because an upstream pricing service
// returns {"prices": null} and the handler indexes into it. The upstream call
// goes through the SDK Transport (net/http RoundTripper boundary) and the
// database call through database/sql over the SDK's SQLDriver wrap of a
// fake pg driver that MUST never be reached during hermetic replay.
//
// MODE=capture boots the upstream plus the app, fires the failing request
// through the real server, and writes a version 2 reproit-backend-capture
// (exchanges plus envelope) to CAPTURE_OUT. Default (server) mode boots ONLY
// the app on $PORT; with REPROIT_REPLAY set the SDK serves the recorded
// exchanges in process, so neither the upstream nor the database exists.
// FIXED=1 applies the fix.
package main

import (
	"context"
	"database/sql"
	"database/sql/driver"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"strconv"
	"time"

	reproit "github.com/reproit/reproit-backend"
)

const (
	upstreamAddr = "127.0.0.1:19941"
	capturePort  = 19940
)

// fixtureDriver is the pg-shaped driver the SDK wraps. Outside capture mode
// a live database would be dialed, so it fails loudly: hermetic replay must
// serve the recorded rows through the SQLDriver connect stub instead.
type fixtureDriver struct{}

func (fixtureDriver) Open(string) (driver.Conn, error) {
	if os.Getenv("MODE") != "capture" {
		return nil, errors.New("live database dialed during hermetic replay")
	}
	return fixtureConn{}, nil
}

type fixtureConn struct{}

func (fixtureConn) Prepare(string) (driver.Stmt, error) {
	return nil, errors.New("fixture driver serves QueryContext only")
}
func (fixtureConn) Close() error              { return nil }
func (fixtureConn) Begin() (driver.Tx, error) { return fixtureTx{}, nil }

func (fixtureConn) QueryContext(_ context.Context, query string,
	args []driver.NamedValue) (driver.Rows, error) {
	symbol := "ACME"
	if len(args) > 0 {
		if text, ok := args[0].Value.(string); ok {
			symbol = text
		}
	}
	return &fixtureRows{symbol: symbol}, nil
}

type fixtureTx struct{}

func (fixtureTx) Commit() error   { return nil }
func (fixtureTx) Rollback() error { return nil }

type fixtureRows struct {
	symbol string
	index  int
}

func (r *fixtureRows) Columns() []string { return []string{"id", "symbol"} }
func (r *fixtureRows) Close() error      { return nil }

func (r *fixtureRows) Next(dest []driver.Value) error {
	if r.index > 0 {
		return io.EOF
	}
	r.index++
	dest[0] = int64(7)
	dest[1] = r.symbol
	return nil
}

// quote is the planted operation: a database/sql lookup, then an upstream
// HTTP call whose null prices field the handler indexes into.
func quote(ctx context.Context, client *http.Client, db *sql.DB,
	upstream, symbol string) (int, any) {
	internal := map[string]any{"error": "internal"}
	var id int64
	var name string
	row := db.QueryRowContext(ctx,
		"SELECT id, symbol FROM issuers WHERE symbol = $1", symbol)
	if err := row.Scan(&id, &name); err != nil {
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

func openDB() (*sql.DB, error) {
	sql.Register("reproit-fixture-pg", &reproit.SQLDriver{Base: fixtureDriver{}})
	return sql.Open("reproit-fixture-pg", "postgres://db.internal/quotes")
}

// quoteHandler serves GET /quote. In capture mode each request runs under a
// capture-envelope trace and the finished capture payload is written to
// CAPTURE_OUT (a file sink instead of a cloud upload, so the fixture needs
// no cloud).
func quoteHandler(client *http.Client, db *sql.DB, captureOut string) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		symbol := r.URL.Query().Get("symbol")
		if symbol == "" {
			symbol = "ACME"
		}
		ctx := r.Context()
		var trace *reproit.BackendTrace
		if captureOut != "" {
			started, err := reproit.Begin(&reproit.TraceContext{
				TraceID:         "cap-money-go-fixture-1",
				Build:           "go-money-fixture",
				CaptureEnvelope: true,
			}, "GET /quote", reproit.BeginOptions{
				Input: reproit.HTTPInput{Query: map[string]any{"symbol": symbol}}.Value(),
			})
			if err == nil {
				trace = started
				ctx = reproit.ContextWithTrace(ctx, trace)
			}
		}
		status, output := quote(ctx, client, db, "http://"+upstreamAddr, symbol)
		if trace != nil {
			if err := writeCapture(trace, captureOut, output, status); err != nil {
				fmt.Fprintln(os.Stderr, "fixture capture:", err)
			}
		}
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(status)
		_ = json.NewEncoder(w).Encode(output)
	}
}

func writeCapture(trace *reproit.BackendTrace, out string, output any, status int) error {
	if err := trace.Finish(output, status, status < 500, true); err != nil {
		return err
	}
	events := trace.Events()
	values := make([]any, 0, len(events))
	for _, event := range events {
		values = append(values, event)
	}
	payload := map[string]any{
		"format":    reproit.CaptureFormat,
		"version":   2,
		"operation": "GET /quote",
		"oracle":    reproit.ServerErrorOracle,
		"envelope":  reproit.DeterminismEnvelope(events[0]["at"]),
		"events":    values,
	}
	return os.WriteFile(out, reproit.CanonicalJSON(payload), 0o600)
}

func captureMode(db *sql.DB) error {
	out := os.Getenv("CAPTURE_OUT")
	if out == "" {
		return fmt.Errorf("CAPTURE_OUT is required in capture mode")
	}
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

	handler := http.NewServeMux()
	handler.HandleFunc("/quote", quoteHandler(reproit.WrapClient(nil), db, out))
	appAddr := "127.0.0.1:" + strconv.Itoa(capturePort)
	app := &http.Server{Addr: appAddr, Handler: handler,
		ReadHeaderTimeout: 5 * time.Second}
	appListener, err := net.Listen("tcp", appAddr)
	if err != nil {
		return err
	}
	go func() { _ = app.Serve(appListener) }()
	defer func() { _ = app.Close() }()

	response, err := http.Get("http://" + appAddr + "/quote?symbol=ACME")
	if err != nil {
		return err
	}
	_, _ = io.Copy(io.Discard, response.Body)
	_ = response.Body.Close()
	fmt.Println("capture fixture status", response.StatusCode)
	return nil
}

func serverMode(db *sql.DB) error {
	reproit.Init()
	client := reproit.WrapClient(nil)
	port := capturePort
	if raw := os.Getenv("PORT"); raw != "" {
		if parsed, err := strconv.Atoi(raw); err == nil {
			port = parsed
		}
	}
	handler := http.NewServeMux()
	handler.HandleFunc("/quote", quoteHandler(client, db, ""))
	server := &http.Server{
		Addr:              "127.0.0.1:" + strconv.Itoa(port),
		Handler:           handler,
		ReadHeaderTimeout: 5 * time.Second,
	}
	fmt.Println("serving on", port)
	return server.ListenAndServe()
}

func main() {
	db, err := openDB()
	if err == nil {
		if os.Getenv("MODE") == "capture" {
			err = captureMode(db)
		} else {
			err = serverMode(db)
		}
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, "go backend fixture:", err)
		os.Exit(1)
	}
}
