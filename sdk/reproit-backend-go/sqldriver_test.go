package reproitbackend

import (
	"context"
	"database/sql"
	"database/sql/driver"
	"encoding/json"
	"io"
	"os"
	"sync/atomic"
	"testing"
)

// fakeDriver is a minimal QueryerContext/ExecerContext driver so the wrap is
// exercised through the real database/sql machinery.
type fakeDriver struct {
	opens atomic.Int64
}

func (d *fakeDriver) Open(string) (driver.Conn, error) {
	d.opens.Add(1)
	return &fakeConn{}, nil
}

type fakeConn struct{}

func (c *fakeConn) Prepare(string) (driver.Stmt, error) {
	return nil, driver.ErrSkip
}
func (c *fakeConn) Close() error              { return nil }
func (c *fakeConn) Begin() (driver.Tx, error) { return replaySQLTx{}, nil }

func (c *fakeConn) QueryContext(_ context.Context, query string,
	args []driver.NamedValue) (driver.Rows, error) {
	if query == "SELECT boom" {
		return nil, &DBError{Message: "relation missing", Code: "42P01"}
	}
	return &fakeRows{}, nil
}

func (c *fakeConn) ExecContext(_ context.Context, query string,
	args []driver.NamedValue) (driver.Result, error) {
	return driver.RowsAffected(1), nil
}

type fakeRows struct{ index int }

func (r *fakeRows) Columns() []string { return []string{"id", "symbol"} }
func (r *fakeRows) Close() error      { return nil }

func (r *fakeRows) Next(dest []driver.Value) error {
	if r.index > 0 {
		return io.EOF
	}
	r.index++
	dest[0] = int64(7)
	dest[1] = "ACME"
	return nil
}

func openWrapped(t *testing.T, name string, base driver.Driver) *sql.DB {
	t.Helper()
	sql.Register(name, &SQLDriver{Base: base})
	db, err := sql.Open(name, "postgres://db.internal/quotes")
	if err != nil {
		t.Fatal(err)
	}
	return db
}

func TestSQLDriverRecordsQueryRowsAndErrors(t *testing.T) {
	db := openWrapped(t, "reproit-test-capture", &fakeDriver{})
	defer db.Close()
	trace, err := Begin(&TraceContext{TraceID: "cap-sql-1", CaptureEnvelope: true},
		"GET /quote", BeginOptions{})
	if err != nil {
		t.Fatal(err)
	}
	ctx := ContextWithTrace(t.Context(), trace)

	var id int64
	var symbol string
	row := db.QueryRowContext(ctx,
		"SELECT id, symbol FROM issuers WHERE symbol = $1", "ACME")
	if err := row.Scan(&id, &symbol); err != nil {
		t.Fatal(err)
	}
	if id != 7 || symbol != "ACME" {
		t.Fatalf("live rows wrong: %d %q", id, symbol)
	}
	if _, err := db.QueryContext(ctx, "SELECT boom"); err == nil {
		t.Fatal("live error was swallowed")
	}

	exchanges := []map[string]any{}
	for _, event := range trace.Events() {
		if exchange, ok := event["exchange"].(map[string]any); ok {
			exchanges = append(exchanges, exchange)
		}
	}
	if len(exchanges) != 2 {
		t.Fatalf("expected two sql exchanges, got %d", len(exchanges))
	}
	if exchanges[0]["protocol"] != "pg" {
		t.Fatalf("protocol wrong: %v", exchanges[0])
	}
	request := exchanges[0]["request"].(map[string]any)
	if request["values"].([]any)[0] != "ACME" {
		t.Fatalf("statement values lost: %v", request)
	}
	response := exchanges[0]["response"].(map[string]any)
	if response["command"] != "SELECT" {
		t.Fatalf("command tag lost: %v", response)
	}
	rows := response["rows"].([]any)
	if len(rows) != 1 || rows[0].(map[string]any)["symbol"] != "ACME" {
		t.Fatalf("recorded rows lost: %v", response)
	}
	failure := exchanges[1]["response"].(map[string]any)["error"].(map[string]any)
	if failure["message"] != "relation missing" || failure["code"] != "42P01" {
		t.Fatalf("recorded sql error lost: %v", failure)
	}
}

func TestReplaySQLConnServesRecordedRowsWithoutABaseDriver(t *testing.T) {
	loaded := loadedCapture(t, replayCapture)
	conn := &replaySQLConn{session: loaded}
	rows, err := conn.QueryContext(t.Context(),
		"SELECT id FROM issuers WHERE symbol = $1",
		[]driver.NamedValue{{Ordinal: 1, Value: "ACME"}})
	if err != nil {
		t.Fatal(err)
	}
	if columns := rows.Columns(); len(columns) != 1 || columns[0] != "id" {
		t.Fatalf("recorded column order lost: %v", columns)
	}
	dest := make([]driver.Value, 1)
	if err := rows.Next(dest); err != nil {
		t.Fatal(err)
	}
	if dest[0] != int64(7) {
		t.Fatalf("recorded row value lost: %v (%T)", dest[0], dest[0])
	}
	if err := rows.Next(dest); err != io.EOF {
		t.Fatalf("row count wrong: %v", err)
	}
}

func TestReplaySQLConnDivergesOnAnUnrecordedStatement(t *testing.T) {
	loaded := loadedCapture(t, replayCapture)
	conn := &replaySQLConn{session: loaded}
	stderr := os.Stderr
	devNull, _ := os.Open(os.DevNull)
	os.Stderr = devNull
	_, err := conn.QueryContext(t.Context(), "SELECT * FROM surprises", nil)
	os.Stderr = stderr
	_ = devNull.Close()
	if err == nil {
		t.Fatal("unrecorded statement did not fail closed")
	}
	if _, ok := err.(*DBError); !ok {
		t.Fatalf("divergence surfaced as the wrong error type: %T", err)
	}
}

func TestSQLDriverExecRecordsRowCount(t *testing.T) {
	db := openWrapped(t, "reproit-test-exec", &fakeDriver{})
	defer db.Close()
	trace, err := Begin(&TraceContext{TraceID: "cap-sql-2", CaptureEnvelope: true},
		"POST /order", BeginOptions{})
	if err != nil {
		t.Fatal(err)
	}
	ctx := ContextWithTrace(t.Context(), trace)
	if _, err := db.ExecContext(ctx,
		"INSERT INTO orders (id) VALUES ($1)", 1); err != nil {
		t.Fatal(err)
	}
	for _, event := range trace.Events() {
		exchange, ok := event["exchange"].(map[string]any)
		if !ok {
			continue
		}
		response := exchange["response"].(map[string]any)
		if response["command"] != "INSERT" {
			t.Fatalf("exec command tag wrong: %v", response)
		}
		if count, _ := response["rowCount"].(json.Number); count.String() != "1" {
			t.Fatalf("exec row count wrong: %v", response)
		}
		return
	}
	t.Fatal("exec recorded no exchange")
}
