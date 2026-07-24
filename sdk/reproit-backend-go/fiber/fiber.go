// Package reproitfiber is the Fiber v2 middleware for reproit-backend-go.
//
// A separate Go module so the core adapter stays free of third-party
// dependencies. Scan-time: inert unless the request carries
// `x-reproit-trace`; the finished trace is returned as the
// `x-reproit-events` response header. Production: pass a Capture and every
// request is traced and handed to the sampler instead. Handlers record
// observed effects via From(c). Every adapter path fails closed:
// instrumentation errors never reach the host app.
package reproitfiber

import (
	"bytes"
	"encoding/json"
	"strings"

	"github.com/gofiber/fiber/v2"
	reproit "github.com/reproit/reproit-backend"
)

const maxBodyBytes = 64 * 1024

const localsKey = "reproit-backend-trace"

// Options configures the middleware. The zero value is a scan-time-only
// adapter with `METHOD /path` operation names.
type Options struct {
	// Capture enables production capture mode; nil keeps scan-time only.
	Capture *reproit.Capture
	// Operation names the traced operation; default `METHOD /path`.
	Operation func(*fiber.Ctx) string
	// Tenant extracts a non-secret tenant identifier; default none.
	Tenant func(*fiber.Ctx) string
	// EffectsComplete asserts the adapter observed every persistent effect.
	EffectsComplete bool
}

// From returns the request's trace recorder, or nil when the request is not
// being traced.
func From(c *fiber.Ctx) *reproit.BackendTrace {
	trace, _ := c.Locals(localsKey).(*reproit.BackendTrace)
	return trace
}

// New builds the middleware: app.Use(reproitfiber.New(reproitfiber.Options{...})).
func New(options Options) fiber.Handler {
	return func(c *fiber.Ctx) error {
		trace, scan := begin(c, options)
		if trace == nil {
			return c.Next()
		}
		c.Locals(localsKey, trace)
		err := c.Next()
		finalize(c, options, trace, scan, err)
		return err
	}
}

// begin starts the trace (fail closed: nil on any defect) and reports
// whether it came from a scan-time header.
func begin(c *fiber.Ctx, options Options) (trace *reproit.BackendTrace, scan bool) {
	defer func() {
		// Fail closed: an instrumentation defect must not break the request.
		if recover() != nil {
			trace, scan = nil, false
		}
	}()
	scanContext := reproit.TraceContextFromHeaders(func(name string) string {
		return c.Get(name)
	})
	context := scanContext
	if context == nil && options.Capture != nil {
		context = options.Capture.Context()
	}
	if context == nil {
		return nil, false
	}
	operation := c.Method() + " " + c.Path()
	if options.Operation != nil {
		operation = options.Operation(c)
	}
	tenant := ""
	if options.Tenant != nil {
		tenant = options.Tenant(c)
	}
	query := map[string]any{}
	c.Context().QueryArgs().VisitAll(func(key, value []byte) {
		appendValue(query, string(key), string(value))
	})
	headers := map[string]any{}
	c.Request().Header.VisitAll(func(key, value []byte) {
		appendValue(headers, string(key), string(value))
	})
	trace, err := reproit.Begin(context, operation, reproit.BeginOptions{
		Tenant: tenant,
		Input: reproit.HTTPInput{
			Body:    decodeJSON(c.Body(), c.Get(fiber.HeaderContentType)),
			Query:   query,
			Headers: headers,
		}.Value(),
	})
	if err != nil {
		return nil, false
	}
	return trace, scanContext != nil
}

// finalize records the return event once the handler chain is done. Fiber
// buffers the whole response, so status, headers, and body are all final.
func finalize(
	c *fiber.Ctx,
	options Options,
	trace *reproit.BackendTrace,
	scan bool,
	handlerErr error,
) {
	defer func() {
		// Oversized or over-long traces drop their header; the response ships.
		_ = recover()
	}()
	if trace.Finished() {
		return
	}
	status := c.Response().StatusCode()
	if handlerErr != nil {
		status = fiber.StatusInternalServerError
		if fiberErr, ok := handlerErr.(*fiber.Error); ok {
			status = fiberErr.Code
		}
	}
	var output any
	if handlerErr == nil {
		kind := string(c.Response().Header.ContentType())
		output = decodeJSON(c.Response().Body(), kind)
	}
	err := trace.Finish(output, status, status < 500, options.EffectsComplete)
	if err != nil {
		return
	}
	if scan {
		if header, err := trace.Header(); err == nil {
			c.Set("x-reproit-events", header)
		}
	} else if options.Capture != nil {
		options.Capture.Record(trace)
	}
}

func appendValue(fields map[string]any, key, value string) {
	prior, present := fields[key]
	if !present {
		fields[key] = value
		return
	}
	if list, ok := prior.([]any); ok {
		fields[key] = append(list, value)
	} else {
		fields[key] = []any{prior, value}
	}
}

func decodeJSON(body []byte, contentType string) any {
	if len(body) == 0 || len(body) > maxBodyBytes ||
		!strings.Contains(contentType, "application/json") {
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
