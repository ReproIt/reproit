// net/http middleware for reproit-backend-go.
//
// Scan-time: inert unless the request carries `x-reproit-trace`; the finished
// trace is returned as the `x-reproit-events` response header. Production:
// pass a Capture and every request is traced and handed to the sampler
// instead. Handlers record observed effects via FromRequest / FromContext.
// Every adapter path fails closed: instrumentation errors never reach the
// host app.
//
// Bodies are buffered up to a fixed cap so the start/return events carry the
// decoded JSON payloads; larger or non-JSON bodies are traced without
// content. Router path parameters are not part of the canonical input here.
package reproitbackend

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"net/http"
	"strings"
)

const maxBodyBytes = 64 * 1024

type traceContextKey struct{}

// FromContext returns the request's trace recorder, or nil when the request
// is not being traced.
func FromContext(ctx context.Context) *BackendTrace {
	trace, _ := ctx.Value(traceContextKey{}).(*BackendTrace)
	return trace
}

// FromRequest returns the request's trace recorder, or nil when the request
// is not being traced.
func FromRequest(r *http.Request) *BackendTrace {
	return FromContext(r.Context())
}

// MiddlewareOptions configures the net/http middleware. The zero value is a
// scan-time-only adapter with `METHOD /path` operation names.
type MiddlewareOptions struct {
	// Capture enables production capture mode; nil keeps scan-time only.
	Capture *Capture
	// Operation names the traced operation; default `METHOD /path`.
	Operation func(*http.Request) string
	// Tenant extracts a non-secret tenant identifier; default none.
	Tenant func(*http.Request) string
	// EffectsComplete asserts the adapter observed every persistent effect.
	EffectsComplete bool
}

// Middleware wraps an http.Handler with the trace adapter.
func Middleware(options MiddlewareOptions) func(http.Handler) http.Handler {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			trace, scan := beginRequest(r, options)
			if trace == nil {
				next.ServeHTTP(w, r)
				return
			}
			r = r.WithContext(context.WithValue(r.Context(), traceContextKey{}, trace))
			recorder := &responseRecorder{writer: w, status: http.StatusOK}
			finalize := func(outputKnown bool) {
				defer func() {
					// Fail closed: oversized or over-long traces drop
					// their header; the response ships regardless.
					_ = recover()
				}()
				if trace.Finished() {
					return
				}
				var output any
				if outputKnown && !recorder.overflow {
					kind := recorder.Header().Get("Content-Type")
					output = decodeJSONBody(recorder.body.Bytes(), kind, true)
				}
				status := recorder.status
				err := trace.Finish(output, status, status < 500, options.EffectsComplete)
				if err != nil {
					return
				}
				if scan {
					if header, err := trace.Header(); err == nil {
						recorder.Header().Set("x-reproit-events", header)
					}
				} else if options.Capture != nil {
					options.Capture.Record(trace)
				}
			}
			recorder.beforeFlush = finalize
			next.ServeHTTP(recorder, r)
			finalize(true)
			recorder.release()
		})
	}
}

// beginRequest starts the trace (fail closed: nil on any defect) and reports
// whether it came from a scan-time header.
func beginRequest(r *http.Request, options MiddlewareOptions) (trace *BackendTrace, scan bool) {
	defer func() {
		// Fail closed: an instrumentation defect must not break the request.
		if recover() != nil {
			trace, scan = nil, false
		}
	}()
	get := func(name string) string { return r.Header.Get(name) }
	scanContext := TraceContextFromHeaders(get)
	context := scanContext
	if context == nil && options.Capture != nil {
		context = options.Capture.Context()
	}
	if context == nil {
		return nil, false
	}
	operation := r.Method + " " + r.URL.Path
	if options.Operation != nil {
		operation = options.Operation(r)
	}
	tenant := ""
	if options.Tenant != nil {
		tenant = options.Tenant(r)
	}
	body, complete := bufferRequestBody(r)
	trace, err := Begin(context, operation, BeginOptions{
		Tenant: tenant,
		Input: HTTPInput{
			Body:    decodeJSONBody(body, r.Header.Get("Content-Type"), complete),
			Query:   multiValues(r.URL.Query()),
			Headers: multiValues(r.Header),
		}.Value(),
	})
	if err != nil {
		return nil, false
	}
	return trace, scanContext != nil
}

// bufferRequestBody reads up to the cap and hands the handler an intact
// body (buffered bytes chained with the unread remainder).
func bufferRequestBody(r *http.Request) (body []byte, complete bool) {
	if r.Body == nil || r.Body == http.NoBody {
		return nil, true
	}
	buffered, err := io.ReadAll(io.LimitReader(r.Body, maxBodyBytes+1))
	complete = err == nil && len(buffered) <= maxBodyBytes
	r.Body = struct {
		io.Reader
		io.Closer
	}{io.MultiReader(bytes.NewReader(buffered), r.Body), r.Body}
	return buffered, complete
}

func decodeJSONBody(body []byte, contentType string, complete bool) any {
	if !complete || len(body) == 0 || !strings.Contains(contentType, "application/json") {
		return nil
	}
	decoder := json.NewDecoder(bytes.NewReader(body))
	decoder.UseNumber()
	var decoded any
	if decoder.Decode(&decoded) != nil {
		return nil
	}
	return decoded
}

// multiValues lowers url.Values / http.Header into the canonical input
// shape: a single string, or a list for repeated parameters.
func multiValues[V ~map[string][]string](values V) map[string]any {
	if len(values) == 0 {
		return nil
	}
	out := make(map[string]any, len(values))
	for key, list := range values {
		if len(list) == 1 {
			out[key] = list[0]
		} else {
			items := make([]any, len(list))
			for index, item := range list {
				items[index] = item
			}
			out[key] = items
		}
	}
	return out
}

// responseRecorder holds the response (status, headers, bounded body) so the
// return event and the `x-reproit-events` header are complete before headers
// flush. Once the body exceeds the cap or the handler flushes, the trace is
// finalized without output and writes stream through.
type responseRecorder struct {
	writer      http.ResponseWriter
	beforeFlush func(outputKnown bool)
	status      int
	body        bytes.Buffer
	overflow    bool
	released    bool
}

func (r *responseRecorder) Header() http.Header { return r.writer.Header() }

func (r *responseRecorder) WriteHeader(status int) {
	// After release the header is already on the wire; a late WriteHeader
	// is superfluous, exactly as in net/http.
	if !r.released {
		r.status = status
	}
}

func (r *responseRecorder) Write(body []byte) (int, error) {
	if r.released {
		return r.writer.Write(body)
	}
	r.body.Write(body)
	if r.body.Len() > maxBodyBytes {
		r.overflow = true
		r.beforeFlush(false)
		r.release()
	}
	return len(body), nil
}

// Flush finalizes early (output unknown) and streams from then on, so
// handlers that flush still get correct scan headers.
func (r *responseRecorder) Flush() {
	if !r.released {
		r.beforeFlush(false)
		r.release()
	}
	if flusher, ok := r.writer.(http.Flusher); ok {
		flusher.Flush()
	}
}

// release writes the held status and body through to the real writer.
func (r *responseRecorder) release() {
	if r.released {
		return
	}
	r.released = true
	r.writer.WriteHeader(r.status)
	if r.body.Len() > 0 {
		_, _ = r.writer.Write(r.body.Bytes())
		r.body.Reset()
	}
}
