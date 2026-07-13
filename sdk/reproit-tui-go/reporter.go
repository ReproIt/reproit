package reproittui

// reporter.go is the embeddable production half of the SDK. A Go TUI app (bubbletea,
// tview, gocui, or a hand-rolled raw-mode dashboard) creates one Reporter, calls
// Observe(...) with each rendered screen, and the SDK:
//
//  1. computes the SAME TUI screen signature the fuzz runner computes (signature.go),
//  2. records a coverage EDGE whenever the structural signature changes (and uses the
//     content fingerprint as the Layer-1 effect token, exactly like the runner),
//  3. batches events and POSTs them to the cloud as the SAME contract every other
//     reproit SDK uses: {appId, sentAt, ctx?, events},
//  4. installs a panic + signal handler that flushes the buffer (with a crash event
//     carrying the crashing screen's signature) before the process dies, so a
//     production crash is reported with the exact state signature to replay locally.
//
// Two embed shapes are offered, matching whatever the app already has:
//
//	(a) Observe(ScreenContents{...})  -- the app hands the SDK its cell grid (or a
//	    pre-rendered contents string); the SDK does NOT touch the terminal. This is
//	    the right path for bubbletea/tview where the framework owns rendering.
//
//	(b) ObserveContents(text, row, col) -- the app hands the SDK the exact
//	    vt100-style contents string it already has. Lowest-level escape hatch.
//
// The cell-grid path (a) reproduces vt100's screen().contents() text model
// byte-for-byte (see ScreenContents.Text), so the signature the embedded app reports
// equals the one the runner computes from the same screen.
//
// No em dashes anywhere, per project rules.

import (
	"bytes"
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"os/signal"
	"runtime/debug"
	"sort"
	"sync"
	"syscall"
	"time"
)

// Cell is one terminal cell. Contents is the cell's grapheme (usually one rune; "" for
// an empty/never-written cell). Wide marks a double-width cell (CJK, some emoji): vt100
// emits the wide cell's contents once and SKIPS the trailing spacer column, so the SDK
// must too (see ScreenContents.Text).
type Cell struct {
	Contents string
	Wide     bool
}

// ScreenContents is the embed-API screen model: a row-major cell grid plus the cursor
// position. It mirrors exactly what the runner's vt100 parser exposes (a Rows x Cols
// grid + screen().cursor_position()). The app fills this from its own renderer each
// frame and hands it to Observe.
//
// If you already have the rendered contents string (e.g. you captured it yourself, or
// your framework gives you one), set Raw instead of Grid and Text() returns it verbatim.
type ScreenContents struct {
	// Grid is row-major: Grid[row][col]. Rows need not all be the same length; missing
	// trailing cells are treated as empty (no contents), exactly like vt100 trailing-
	// space trimming.
	Grid [][]Cell
	// CursorRow, CursorCol are the 0-based cursor cell, matching vt100
	// screen().cursor_position() (which tui.rs reads as the (row, col) tuple).
	CursorRow uint16
	CursorCol uint16
	// Raw, if non-empty, is used as the contents string verbatim and Grid is ignored.
	Raw string
}

// Text renders the grid to the SAME contents string vt100's screen().contents()
// produces, byte-for-byte. The rules, taken from vt100-0.15 grid/row write_contents:
//
//   - For each row, walk cells left to right. Emit a space for each gap column that
//     precedes a non-empty cell; emit the cell's contents for a non-empty cell. A wide
//     cell emits its contents and the next column (its spacer) is skipped. Trailing
//     empty cells emit NOTHING (per-row trailing whitespace is trimmed).
//   - Rows are joined with '\n'.
//   - Finally, all trailing '\n' are stripped from the whole string (trailing blank
//     rows are trimmed).
//
// This is the model the runner hashes, so reproducing it exactly is what makes the
// embedded signature equal the runner signature.
func (s ScreenContents) Text() string {
	if s.Raw != "" || s.Grid == nil {
		return s.Raw
	}
	var buf bytes.Buffer
	for _, row := range s.Grid {
		// find the last non-empty cell so trailing empties are dropped
		last := -1
		for i, c := range row {
			if c.Contents != "" {
				last = i
			}
		}
		col := 0
		for col <= last {
			c := row[col]
			if c.Contents != "" {
				buf.WriteString(c.Contents)
				if c.Wide {
					col += 2 // skip the spacer column the wide glyph occupies
					continue
				}
			} else {
				buf.WriteByte(' ') // a gap before a later non-empty cell
			}
			col++
		}
		buf.WriteByte('\n')
	}
	out := buf.Bytes()
	for len(out) > 0 && out[len(out)-1] == '\n' {
		out = out[:len(out)-1]
	}
	return string(out)
}

// Event is one reported telemetry record. Mirrors the {kind, ...} event shape the
// other SDKs emit (see reproit-web.js _emit / edge): edges carry from/action/to and
// the human label set; the crash event carries the crashing signature and message.
type Event struct {
	Kind   string   `json:"kind"`             // "session" | "edge" | "state" | "crash"
	T      int64    `json:"t"`                // ms epoch
	From   string   `json:"from,omitempty"`   // edge: previous signature
	Action string   `json:"action,omitempty"` // edge: the action that caused the move
	To     string   `json:"to,omitempty"`     // edge/state/crash: the signature
	Sig    string   `json:"sig,omitempty"`    // state/crash: the signature
	Labels []string `json:"labels,omitempty"` // display-only word set (never the sig)
	Error  string   `json:"error,omitempty"`  // crash: the panic/signal message
}

// batch is the wire contract POSTed to the endpoint: identical to every other reproit
// SDK ({appId, sentAt, ctx?, events}).
type batch struct {
	AppID  string                 `json:"appId"`
	SentAt int64                  `json:"sentAt"`
	Ctx    map[string]interface{} `json:"ctx,omitempty"`
	Events []Event                `json:"events"`
}

// Config configures a Reporter.
type Config struct {
	AppID    string                 // application identifier (required for cloud reporting)
	Endpoint string                 // POST target; if "", events go to OnEvent / are dropped
	Ctx      map[string]interface{} // optional static context attached to every batch
	OnEvent  func(Event)            // optional local sink (testing / custom transport)
	// FlushAt is the buffered-event count that triggers an automatic flush (default 50,
	// matching the web SDK).
	FlushAt int
	// HTTPClient lets the host inject a client (timeouts, proxy). Defaults to a client
	// with a short timeout so reporting never blocks the app.
	HTTPClient *http.Client
	// RedactLabels drops the human label set from edge events (the labels can contain
	// raw on-screen words). Signatures are always locale-invariant and safe.
	RedactLabels bool
}

// Reporter is the embeddable session/coverage/crash reporter. Safe for concurrent use.
type Reporter struct {
	cfg Config

	mu         sync.Mutex
	buf        []Event
	cur        string // current structural signature
	curFP      string // current content fingerprint (Layer-1 effect token, ephemeral)
	started    bool
	crashHook  func()
	invariants map[string]func() error // app-declared invariants, idempotent by id
}

// New creates and starts a Reporter, emitting an initial "session" event.
func New(cfg Config) *Reporter {
	if cfg.FlushAt <= 0 {
		cfg.FlushAt = 50
	}
	if cfg.HTTPClient == nil {
		cfg.HTTPClient = &http.Client{Timeout: 5 * time.Second}
	}
	InstallCausalHTTP(cfg.Endpoint)
	r := &Reporter{cfg: cfg}
	r.emit(Event{Kind: "session"})
	r.started = true
	return r
}

// Observe records the current rendered screen. If its STRUCTURAL signature differs
// from the last one, an edge event is recorded (the runner's coverage edge). The
// CONTENT fingerprint is tracked too: a value-only change (same skeleton, different
// on-screen number) is detected as an effect, exactly as the runner does, but it is
// ephemeral and never becomes the canonical state identity.
//
// action is the user/agent action that produced this screen (e.g. "key:Down",
// "key:Enter"); pass "" for an unattributed observation (it records as "auto").
func (r *Reporter) Observe(screen ScreenContents, action string) {
	text := screen.Text()
	r.ObserveContents(text, screen.CursorRow, screen.CursorCol, action)
}

// ObserveContents is the low-level path: the app hands the exact vt100-style contents
// string plus the 0-based cursor cell. Used by Observe and available directly for apps
// that already hold a contents string.
func (r *Reporter) ObserveContents(contents string, cursorRow, cursorCol uint16, action string) {
	sig := StructuralSig(contents, cursorRow, cursorCol)
	fp := ContentFingerprint(contents, cursorRow, cursorCol)

	// App-invariant oracle (SDK-self-triggered): under the fuzzer, evaluate the
	// app's registered predicates against this state and report failures on the
	// channel the TUI backend scrapes. No-op in production.
	r.reportInvariants(sig)

	r.mu.Lock()
	from := r.cur
	sigChanged := sig != r.cur
	r.cur = sig
	r.curFP = fp
	r.mu.Unlock()

	if !sigChanged {
		// No structural change. (A value-only effect is real but does not open a new
		// coverage edge; the runner records edges only on signature change, so we
		// match that and keep the cloud graph identical.)
		return
	}
	if action == "" {
		action = "auto"
	}
	var labels []string
	if !r.cfg.RedactLabels {
		labels = LabelsOf(contents)
	}
	r.emit(Event{Kind: "edge", From: from, Action: action, To: sig, Labels: labels})
}

// CurrentSig returns the last observed structural signature (the state to replay).
func (r *Reporter) CurrentSig() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.cur
}

// Invariant registers an app invariant: a predicate the app declares that must
// hold in EVERY visited state. It returns nil when it HOLDS, or a non-nil error
// (or panics) when it is VIOLATED. Under the fuzzer the SDK evaluates every
// registered invariant on each Observe and reports the failures for the runner
// to turn into `invariant` findings; in production the registry is INERT
// (evaluated only under the fuzzer), so it is zero-overhead until a run
// reproduces it. Registration is idempotent by id, so re-registering an id
// replaces it. Returns the Reporter for chaining.
func (r *Reporter) Invariant(id string, predicate func() error) *Reporter {
	if id == "" || predicate == nil {
		return r
	}
	r.mu.Lock()
	if r.invariants == nil {
		r.invariants = map[string]func() error{}
	}
	r.invariants[id] = predicate
	r.mu.Unlock()
	return r
}

// evalInvariant runs one predicate, returning its failure message and whether it
// was VIOLATED. A returned error is a violation (err.Error()); a panic is also a
// violation (the panic text), mirroring the web SDK's "throws => violated".
func evalInvariant(predicate func() error) (message string, violated bool) {
	defer func() {
		if rec := recover(); rec != nil {
			message = fmt.Sprintf("%v", rec)
			violated = true
		}
	}()
	if err := predicate(); err != nil {
		return err.Error(), true
	}
	return "", false
}

// reportInvariants evaluates every registered invariant and, ONLY under the
// fuzzer (the REPROIT_INVARIANT_FILE env var the TUI backend sets is present and
// names a file, which is also the fuzzer-detection gate), appends one marker line
//
//	REPROIT_INVARIANT {"sig":"<sig>","items":[{"id","message"}...]}
//
// listing the VIOLATED invariants to that file. The TUI backend scrapes the file
// and re-emits each as EXPLORE:INVARIANT. A file (not stderr) is the channel
// because a TUI's stdout/stderr ARE its rendered frames in the PTY (see
// crates/reproit/src/backends/tui.rs). Silent when the registry is empty or every
// invariant held (no empty-items line); inert in production (env var unset).
func (r *Reporter) reportInvariants(sig string) {
	path := os.Getenv("REPROIT_INVARIANT_FILE")
	if path == "" {
		return
	}
	r.mu.Lock()
	ids := make([]string, 0, len(r.invariants))
	for id := range r.invariants {
		ids = append(ids, id)
	}
	preds := make(map[string]func() error, len(r.invariants))
	for id, p := range r.invariants {
		preds[id] = p
	}
	r.mu.Unlock()
	if len(ids) == 0 {
		return
	}
	sort.Strings(ids) // stable marker output (map order is random)

	type item struct {
		ID      string `json:"id"`
		Message string `json:"message"`
	}
	var items []item
	for _, id := range ids {
		if msg, violated := evalInvariant(preds[id]); violated {
			items = append(items, item{ID: id, Message: msg})
		}
	}
	if len(items) == 0 {
		return
	}
	payload := struct {
		Sig   string `json:"sig"`
		Items []item `json:"items"`
	}{Sig: sig, Items: items}
	b, err := json.Marshal(payload)
	if err != nil {
		return
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return
	}
	defer f.Close()
	fmt.Fprintf(f, "REPROIT_INVARIANT %s\n", b)
}

// emit appends an event, stamps it, fans out to OnEvent, and auto-flushes at the
// threshold. Internal lock-free callers must not hold r.mu.
func (r *Reporter) emit(ev Event) {
	ev.T = time.Now().UnixMilli()
	if r.cfg.OnEvent != nil {
		func() {
			defer func() { _ = recover() }() // a bad sink must never break reporting
			r.cfg.OnEvent(ev)
		}()
	}
	r.mu.Lock()
	r.buf = append(r.buf, ev)
	n := len(r.buf)
	r.mu.Unlock()
	if n >= r.cfg.FlushAt {
		r.Flush()
	}
}

// Flush sends the buffered events to the endpoint as one batch and clears the buffer.
// Best-effort: a transport failure never panics or blocks the app meaningfully.
func (r *Reporter) Flush() {
	r.mu.Lock()
	if len(r.buf) == 0 {
		r.mu.Unlock()
		return
	}
	events := r.buf
	r.buf = nil
	r.mu.Unlock()

	b := batch{AppID: r.cfg.AppID, SentAt: time.Now().UnixMilli(), Ctx: r.cfg.Ctx, Events: events}
	body, err := json.Marshal(b)
	if err != nil {
		return
	}
	if r.cfg.Endpoint == "" {
		return // no endpoint: OnEvent already saw each event; nothing to POST
	}
	req, err := http.NewRequest(http.MethodPost, r.cfg.Endpoint, bytes.NewReader(body))
	if err != nil {
		return
	}
	req.Header.Set("Content-Type", "application/json")
	resp, err := r.cfg.HTTPClient.Do(req)
	if err != nil {
		return
	}
	_ = resp.Body.Close()
}

// ReportCrash records a crash event carrying the CURRENT signature (the state to
// replay) and the message, then flushes synchronously. Call this from a recovered
// panic, or let InstallCrashHandler do it for you.
func (r *Reporter) ReportCrash(message string) {
	r.mu.Lock()
	sig := r.cur
	r.mu.Unlock()
	r.emit(Event{Kind: "crash", Sig: sig, To: sig, Error: message})
	r.Flush()
}

// InstallCrashHandler installs a SIGINT/SIGTERM/SIGSEGV/SIGABRT handler that reports a
// crash and flushes before the process exits, and returns a deferrable recover-and-
// report function for panics. Typical embed:
//
//	r := reproittui.New(cfg)
//	defer r.InstallCrashHandler()() // installs signal handler; the returned fn recovers panics
//
// The returned function recovers any in-flight panic, reports it (with the current
// signature and the panic value + stack), flushes, then RE-PANICS so the app's own
// crash semantics are preserved. The signal handler reports asynchronously-safe info
// (no allocation-heavy work in the handler beyond what net/http needs) and re-raises
// the signal with the default disposition so the OS still sees the real exit.
func (r *Reporter) InstallCrashHandler() func() {
	sigc := make(chan os.Signal, 1)
	signal.Notify(sigc, syscall.SIGINT, syscall.SIGTERM, syscall.SIGSEGV, syscall.SIGABRT)
	go func() {
		s := <-sigc
		r.ReportCrash("signal: " + s.String())
		// restore default disposition and re-raise so the OS exit code is honest
		signal.Stop(sigc)
		if p, err := os.FindProcess(os.Getpid()); err == nil {
			_ = p.Signal(s)
		}
	}()

	return func() {
		if rec := recover(); rec != nil {
			msg := "panic: " + sprint(rec) + "\n" + string(debug.Stack())
			r.ReportCrash(msg)
			panic(rec) // preserve the app's crash semantics
		}
	}
}

// sprint stringifies a recovered panic value without pulling in fmt at the call site.
func sprint(v interface{}) string {
	switch x := v.(type) {
	case string:
		return x
	case error:
		return x.Error()
	default:
		b, _ := json.Marshal(x)
		return string(b)
	}
}
