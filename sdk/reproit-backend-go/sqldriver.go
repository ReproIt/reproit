// database/sql driver wrap for reproit-backend-go.
//
// SQLDriver decorates any database/sql driver at the driver.Driver boundary,
// the Go analogue of the Node reference wrapping `pg.Client.prototype.query`:
// statements executed through database/sql are recorded on the ambient trace
// as `pg`-shaped exchanges (text, values, command, rowCount, rows), and with
// REPROIT_REPLAY set the SAME driver serves the recorded results without ever
// opening the underlying driver: Open returns a stub connection, so the app
// boots with the database down.
//
// Coverage is the context-carrying surface database/sql actually uses:
// QueryContext / ExecContext on the connection and on prepared statements.
// The context is where the ambient trace lives, so the non-context forms
// (driver.Stmt Exec/Query, driver.Queryer) pass through unrecorded rather
// than half-recorded, exactly as the Node wrapper passes exotic query shapes
// through. Named gap: the driver API exposes no server command tag, so the
// recorded `command` is derived from the statement's leading verb.
package reproitbackend

import (
	"context"
	"database/sql/driver"
	"encoding/json"
	"errors"
	"io"
	"strings"
	"time"
)

// SQLDriver wraps Base so database/sql traffic crosses the exchange
// boundary. Register it under its own name:
//
//	sql.Register("reproit-pg", &reproitbackend.SQLDriver{Base: pqDriver})
//	db, err := sql.Open("reproit-pg", dsn)
type SQLDriver struct {
	Base driver.Driver
}

// Open implements driver.Driver. In replay mode the base driver is never
// touched: the returned stub serves recorded exchanges, so the app boots
// with the database stopped.
func (d *SQLDriver) Open(dsn string) (driver.Conn, error) {
	if replay := session(); replay != nil {
		return &replaySQLConn{session: replay}, nil
	}
	if d.Base == nil {
		return nil, errors.New("reproit: SQLDriver has no base driver")
	}
	conn, err := d.Base.Open(dsn)
	if err != nil {
		return nil, err
	}
	return &captureSQLConn{conn: conn}, nil
}

// sqlCommandTag derives the recorded command from the statement's leading
// verb: the driver API has no server command tag (named gap vs Node's pg).
func sqlCommandTag(text string) string {
	fields := strings.Fields(text)
	if len(fields) == 0 {
		return ""
	}
	return strings.ToUpper(fields[0])
}

// namedValuesToAny lowers driver arguments into the recorded values list.
// Byte slices become strings so text parameters do not turn into base64.
func namedValuesToAny(args []driver.NamedValue) []any {
	if len(args) == 0 {
		return nil
	}
	values := make([]any, 0, len(args))
	for _, arg := range args {
		values = append(values, driverValueToAny(arg.Value))
	}
	return values
}

func driverValueToAny(value driver.Value) any {
	switch typed := value.(type) {
	case []byte:
		return string(typed)
	case time.Time:
		return typed.Format(time.RFC3339Nano)
	default:
		return typed
	}
}

// recordSQLExchange writes one statement exchange onto the ambient trace,
// mirroring RunDB's shape. Fails closed and counts, never breaks the query.
func recordSQLExchange(ctx context.Context, text string, values []any,
	outcome DBOutcome, failure error) {
	trace := FromContext(ctx)
	if trace == nil {
		return
	}
	defer func() {
		if recover() != nil {
			exchangeCounters.failed.Add(1)
		}
	}()
	request := map[string]any{"text": text}
	if len(values) > 0 {
		request["values"] = values
	}
	err := trace.Exchange(dbEffectKind(text), ExchangeOptions{
		Resource: "pg",
		Key:      truncate(text, 256),
		Exchange: map[string]any{
			"protocol": "pg",
			"request":  request,
			"response": dbOutcomeValue(outcome, failure),
		},
	})
	if err != nil {
		exchangeCounters.failed.Add(1)
		return
	}
	exchangeCounters.captured.Add(1)
}

// --- capture side -----------------------------------------------------------

type captureSQLConn struct {
	conn driver.Conn
}

func (c *captureSQLConn) Prepare(query string) (driver.Stmt, error) {
	stmt, err := c.conn.Prepare(query)
	if err != nil {
		return nil, err
	}
	return &captureSQLStmt{stmt: stmt, query: query}, nil
}

func (c *captureSQLConn) Close() error { return c.conn.Close() }

func (c *captureSQLConn) Begin() (driver.Tx, error) {
	//nolint:staticcheck // pass-through of the legacy interface method.
	return c.conn.Begin()
}

func (c *captureSQLConn) BeginTx(ctx context.Context,
	opts driver.TxOptions) (driver.Tx, error) {
	if beginner, ok := c.conn.(driver.ConnBeginTx); ok {
		return beginner.BeginTx(ctx, opts)
	}
	return c.conn.Begin()
}

func (c *captureSQLConn) Ping(ctx context.Context) error {
	if pinger, ok := c.conn.(driver.Pinger); ok {
		return pinger.Ping(ctx)
	}
	return nil
}

func (c *captureSQLConn) QueryContext(ctx context.Context, query string,
	args []driver.NamedValue) (driver.Rows, error) {
	queryer, ok := c.conn.(driver.QueryerContext)
	if !ok {
		return nil, driver.ErrSkip
	}
	rows, err := queryer.QueryContext(ctx, query, args)
	values := namedValuesToAny(args)
	if err != nil {
		recordSQLExchange(ctx, query, values, DBOutcome{}, err)
		return nil, err
	}
	return newRecordingRows(ctx, query, values, rows), nil
}

func (c *captureSQLConn) ExecContext(ctx context.Context, query string,
	args []driver.NamedValue) (driver.Result, error) {
	execer, ok := c.conn.(driver.ExecerContext)
	if !ok {
		return nil, driver.ErrSkip
	}
	result, err := execer.ExecContext(ctx, query, args)
	values := namedValuesToAny(args)
	if err != nil {
		recordSQLExchange(ctx, query, values, DBOutcome{}, err)
		return nil, err
	}
	affected := int64(0)
	if result != nil {
		if count, countErr := result.RowsAffected(); countErr == nil && count > 0 {
			affected = count
		}
	}
	recordSQLExchange(ctx, query, values, DBOutcome{
		Command: sqlCommandTag(query), RowCount: uint64(affected),
	}, nil)
	return result, nil
}

func (c *captureSQLConn) PrepareContext(ctx context.Context,
	query string) (driver.Stmt, error) {
	if preparer, ok := c.conn.(driver.ConnPrepareContext); ok {
		stmt, err := preparer.PrepareContext(ctx, query)
		if err != nil {
			return nil, err
		}
		return &captureSQLStmt{stmt: stmt, query: query}, nil
	}
	return c.Prepare(query)
}

// captureSQLStmt records the context-carrying statement forms; the
// context-less Exec/Query have no ambient trace and pass through unrecorded.
type captureSQLStmt struct {
	stmt  driver.Stmt
	query string
}

func (s *captureSQLStmt) Close() error  { return s.stmt.Close() }
func (s *captureSQLStmt) NumInput() int { return s.stmt.NumInput() }

func (s *captureSQLStmt) Exec(args []driver.Value) (driver.Result, error) {
	//nolint:staticcheck // pass-through of the legacy interface method.
	return s.stmt.Exec(args)
}

func (s *captureSQLStmt) Query(args []driver.Value) (driver.Rows, error) {
	//nolint:staticcheck // pass-through of the legacy interface method.
	return s.stmt.Query(args)
}

func (s *captureSQLStmt) QueryContext(ctx context.Context,
	args []driver.NamedValue) (driver.Rows, error) {
	queryer, ok := s.stmt.(driver.StmtQueryContext)
	if !ok {
		return nil, driver.ErrSkip
	}
	rows, err := queryer.QueryContext(ctx, args)
	values := namedValuesToAny(args)
	if err != nil {
		recordSQLExchange(ctx, s.query, values, DBOutcome{}, err)
		return nil, err
	}
	return newRecordingRows(ctx, s.query, values, rows), nil
}

func (s *captureSQLStmt) ExecContext(ctx context.Context,
	args []driver.NamedValue) (driver.Result, error) {
	execer, ok := s.stmt.(driver.StmtExecContext)
	if !ok {
		return nil, driver.ErrSkip
	}
	result, err := execer.ExecContext(ctx, args)
	values := namedValuesToAny(args)
	if err != nil {
		recordSQLExchange(ctx, s.query, values, DBOutcome{}, err)
		return nil, err
	}
	affected := int64(0)
	if result != nil {
		if count, countErr := result.RowsAffected(); countErr == nil && count > 0 {
			affected = count
		}
	}
	recordSQLExchange(ctx, s.query, values, DBOutcome{
		Command: sqlCommandTag(s.query), RowCount: uint64(affected),
	}, nil)
	return result, nil
}

// recordingRows tees result rows as the app iterates them, then records the
// exchange exactly once when iteration completes (EOF or Close). Rows past
// the maxDBRows cap are counted in rowCount but marked truncated, matching
// the Node wrapper's bound.
type recordingRows struct {
	ctx      context.Context
	query    string
	values   []any
	rows     driver.Rows
	columns  []string
	seen     []any
	total    uint64
	recorded bool
}

func newRecordingRows(ctx context.Context, query string, values []any,
	rows driver.Rows) *recordingRows {
	return &recordingRows{
		ctx: ctx, query: query, values: values, rows: rows,
		columns: rows.Columns(),
	}
}

func (r *recordingRows) Columns() []string { return r.rows.Columns() }

func (r *recordingRows) Next(dest []driver.Value) error {
	err := r.rows.Next(dest)
	if err != nil {
		if errors.Is(err, io.EOF) {
			r.record(nil)
		} else {
			r.record(err)
		}
		return err
	}
	r.total++
	if len(r.seen) < maxDBRows {
		row := make(map[string]any, len(r.columns))
		for index, column := range r.columns {
			if index < len(dest) {
				row[column] = driverValueToAny(dest[index])
			}
		}
		r.seen = append(r.seen, row)
	}
	return nil
}

func (r *recordingRows) Close() error {
	r.record(nil)
	return r.rows.Close()
}

func (r *recordingRows) record(failure error) {
	if r.recorded {
		return
	}
	r.recorded = true
	if failure != nil {
		recordSQLExchange(r.ctx, r.query, r.values, DBOutcome{}, failure)
		return
	}
	recordSQLExchange(r.ctx, r.query, r.values, DBOutcome{
		Command:  sqlCommandTag(r.query),
		RowCount: r.total,
		Rows:     r.seen,
	}, nil)
}

// --- replay side ------------------------------------------------------------

// replaySQLConn serves recorded exchanges. It is also the connect stub: it
// exists without any live connection, so the app boots with the database
// down and every un-recorded statement fails closed as a divergence.
type replaySQLConn struct {
	session *replaySession
}

func (c *replaySQLConn) Prepare(query string) (driver.Stmt, error) {
	return &replaySQLStmt{conn: c, query: query}, nil
}

func (c *replaySQLConn) Close() error              { return nil }
func (c *replaySQLConn) Begin() (driver.Tx, error) { return replaySQLTx{}, nil }

func (c *replaySQLConn) BeginTx(context.Context, driver.TxOptions) (driver.Tx, error) {
	return replaySQLTx{}, nil
}

func (c *replaySQLConn) Ping(context.Context) error { return nil }

func (c *replaySQLConn) QueryContext(_ context.Context, query string,
	args []driver.NamedValue) (driver.Rows, error) {
	return c.query(query, namedValuesToAny(args))
}

func (c *replaySQLConn) ExecContext(_ context.Context, query string,
	args []driver.NamedValue) (driver.Result, error) {
	return c.exec(query, namedValuesToAny(args))
}

func (c *replaySQLConn) query(text string, values []any) (driver.Rows, error) {
	_, _, rows, err := c.session.serveDBExchange(text, values)
	if err != nil {
		return nil, err
	}
	return newReplayRows(rows), nil
}

func (c *replaySQLConn) exec(text string, values []any) (driver.Result, error) {
	_, rowCount, _, err := c.session.serveDBExchange(text, values)
	if err != nil {
		return nil, err
	}
	return driver.RowsAffected(rowCount), nil
}

type replaySQLStmt struct {
	conn  *replaySQLConn
	query string
}

func (s *replaySQLStmt) Close() error  { return nil }
func (s *replaySQLStmt) NumInput() int { return -1 }

func (s *replaySQLStmt) Exec(args []driver.Value) (driver.Result, error) {
	return s.conn.exec(s.query, valuesToAny(args))
}

func (s *replaySQLStmt) Query(args []driver.Value) (driver.Rows, error) {
	return s.conn.query(s.query, valuesToAny(args))
}

func (s *replaySQLStmt) ExecContext(_ context.Context,
	args []driver.NamedValue) (driver.Result, error) {
	return s.conn.exec(s.query, namedValuesToAny(args))
}

func (s *replaySQLStmt) QueryContext(_ context.Context,
	args []driver.NamedValue) (driver.Rows, error) {
	return s.conn.query(s.query, namedValuesToAny(args))
}

func valuesToAny(args []driver.Value) []any {
	if len(args) == 0 {
		return nil
	}
	values := make([]any, 0, len(args))
	for _, arg := range args {
		values = append(values, driverValueToAny(arg))
	}
	return values
}

type replaySQLTx struct{}

func (replaySQLTx) Commit() error   { return nil }
func (replaySQLTx) Rollback() error { return nil }

// replayRows exposes recorded rows positionally. Column order comes from the
// recorded rows' own key order (the ordered decode preserves the capture
// file), so Scan by position sees what production saw.
type replayRows struct {
	columns []string
	rows    []any
	index   int
}

func newReplayRows(rows []any) *replayRows {
	served := &replayRows{rows: rows}
	if len(rows) > 0 {
		if first, ok := rows[0].(omap); ok {
			for _, entry := range first {
				served.columns = append(served.columns, entry.key)
			}
		}
	}
	return served
}

func (r *replayRows) Columns() []string { return r.columns }
func (r *replayRows) Close() error      { return nil }

func (r *replayRows) Next(dest []driver.Value) error {
	if r.index >= len(r.rows) {
		return io.EOF
	}
	row := r.rows[r.index]
	r.index++
	for position, column := range r.columns {
		if position >= len(dest) {
			break
		}
		value := fieldOr(row, column)
		if value == absent {
			value = nil
		}
		dest[position] = replayDriverValue(value)
	}
	return nil
}

// replayDriverValue lowers a recorded JSON value into a driver.Value:
// integers stay integral, structures re-encode as their recorded JSON text.
func replayDriverValue(value any) driver.Value {
	switch typed := value.(type) {
	case nil, bool, string:
		return typed
	case json.Number:
		if integer, err := typed.Int64(); err == nil {
			return integer
		}
		if float, err := typed.Float64(); err == nil {
			return float
		}
		return typed.String()
	default:
		return string(nodeJSON(typed))
	}
}
