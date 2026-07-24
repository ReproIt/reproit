/*
 * Servlet filter for reproit-backend-java (jakarta.servlet, any container;
 * Spring Boot registers it via FilterRegistrationBean, see README.md).
 *
 * Scan-time: inert unless the request carries `x-reproit-trace`; the finished
 * trace is returned as the `x-reproit-events` response header. Production:
 * pass a Capture and every request is traced and handed to the sampler
 * instead. Handlers record observed effects via the `reproit` request
 * attribute. Every adapter path fails closed: instrumentation errors never
 * reach the host app.
 *
 * Bodies are buffered up to a fixed cap so the start/return events carry the
 * decoded JSON payloads; larger or non-JSON bodies are traced without
 * content. The response body is held (bounded) so the return event and the
 * `x-reproit-events` header are complete before anything is committed; over
 * the cap the trace finishes without output and bytes stream through. Path
 * parameters are matched after the filter runs, so they are not part of the
 * canonical input here.
 */
package dev.reproit.backend;

import jakarta.servlet.Filter;
import jakarta.servlet.FilterChain;
import jakarta.servlet.ReadListener;
import jakarta.servlet.ServletException;
import jakarta.servlet.ServletInputStream;
import jakarta.servlet.ServletOutputStream;
import jakarta.servlet.ServletRequest;
import jakarta.servlet.ServletResponse;
import jakarta.servlet.WriteListener;
import jakarta.servlet.http.HttpServletRequest;
import jakarta.servlet.http.HttpServletRequestWrapper;
import jakarta.servlet.http.HttpServletResponse;
import jakarta.servlet.http.HttpServletResponseWrapper;
import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStreamWriter;
import java.io.PrintWriter;
import java.io.SequenceInputStream;
import java.net.URLDecoder;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.function.Function;

public final class ReproitFilter implements Filter {
    static final int MAX_BODY_BYTES = 64 * 1024;
    public static final String REQUEST_ATTRIBUTE = "reproit";

    private final Capture capture;
    private Function<HttpServletRequest, String> operation;
    private Function<HttpServletRequest, String> tenant;
    private boolean effectsComplete = false;

    public ReproitFilter() {
        this(null);
    }

    public ReproitFilter(Capture capture) {
        this.capture = capture;
    }

    public ReproitFilter operation(Function<HttpServletRequest, String> value) {
        this.operation = value;
        return this;
    }

    public ReproitFilter tenant(Function<HttpServletRequest, String> value) {
        this.tenant = value;
        return this;
    }

    public ReproitFilter effectsComplete(boolean value) {
        this.effectsComplete = value;
        return this;
    }

    @Override
    public void doFilter(ServletRequest rawRequest, ServletResponse rawResponse, FilterChain chain)
            throws IOException, ServletException {
        if (!(rawRequest instanceof HttpServletRequest request)
                || !(rawResponse instanceof HttpServletResponse response)) {
            chain.doFilter(rawRequest, rawResponse);
            return;
        }
        BackendTrace trace;
        TraceContext scanContext;
        BufferedRequest buffered;
        try {
            scanContext = BackendTrace.traceContextFromHeaders(request::getHeader);
            TraceContext context = scanContext;
            if (context == null && capture != null) context = capture.context();
            if (context == null) {
                chain.doFilter(request, response);
                return;
            }
            buffered = new BufferedRequest(request);
            String name = operation != null
                ? operation.apply(request)
                : request.getMethod() + " " + request.getRequestURI();
            trace = BackendTrace.begin(context, name, new BackendTrace.Options()
                .tenant(tenant != null ? tenant.apply(request) : null)
                .input(BackendTrace.httpInput(
                    decodeJson(buffered.held(), request.getContentType(), buffered.complete()),
                    null,
                    queryValues(request.getQueryString()),
                    headerValues(request))));
            request.setAttribute(REQUEST_ATTRIBUTE, trace);
        } catch (RuntimeException ignored) {
            // Fail closed: an instrumentation defect must not break the request.
            chain.doFilter(request, response);
            return;
        }
        HeldResponse held =
            new HeldResponse(response, trace, scanContext != null ? null : capture);
        try {
            chain.doFilter(buffered, held);
            held.release(held.getStatus(), true);
        } catch (IOException | ServletException | RuntimeException failure) {
            held.release(500, false);
            throw failure;
        }
    }

    static Object decodeJson(byte[] body, String contentType, boolean complete) {
        if (!complete || body == null || body.length == 0) return null;
        if (contentType == null || !contentType.contains("application/json")) return null;
        try {
            return Json.parse(new String(body, StandardCharsets.UTF_8));
        } catch (RuntimeException invalid) {
            return null;
        }
    }

    // Decoded query values; repeated parameters become lists.
    static Map<String, Object> queryValues(String queryString) {
        Map<String, Object> values = new LinkedHashMap<>();
        if (queryString == null || queryString.isEmpty()) return values;
        for (String pair : queryString.split("&", -1)) {
            if (pair.isEmpty()) continue;
            int split = pair.indexOf('=');
            String key = split < 0 ? pair : pair.substring(0, split);
            String value = split < 0 ? "" : pair.substring(split + 1);
            try {
                key = URLDecoder.decode(key, StandardCharsets.UTF_8);
                value = URLDecoder.decode(value, StandardCharsets.UTF_8);
            } catch (IllegalArgumentException undecodable) {
                continue;
            }
            merge(values, key, value);
        }
        return values;
    }

    // Lowercased header values; repeated headers become lists.
    static Map<String, Object> headerValues(HttpServletRequest request) {
        Map<String, Object> headers = new LinkedHashMap<>();
        for (String name : Collections.list(request.getHeaderNames())) {
            String key = name.toLowerCase(Locale.ROOT);
            if (headers.containsKey(key)) continue;
            List<String> values = Collections.list(request.getHeaders(name));
            if (values.isEmpty()) continue;
            headers.put(key, values.size() == 1 ? values.get(0) : new ArrayList<>(values));
        }
        return headers;
    }

    @SuppressWarnings("unchecked")
    private static void merge(Map<String, Object> values, String key, String value) {
        Object prior = values.get(key);
        if (prior == null && !values.containsKey(key)) {
            values.put(key, value);
        } else if (prior instanceof List) {
            ((List<Object>) prior).add(value);
        } else {
            List<Object> list = new ArrayList<>();
            list.add(prior);
            list.add(value);
            values.put(key, list);
        }
    }

    /**
     * Pre-reads up to MAX_BODY_BYTES of the request body so the start event
     * can carry the decoded JSON payload, then replays the held bytes (plus
     * any unread remainder) to the servlet. Memory stays bounded.
     */
    private static final class BufferedRequest extends HttpServletRequestWrapper {
        private final byte[] held;
        private final boolean complete;
        private final InputStream replay;

        BufferedRequest(HttpServletRequest request) throws IOException {
            super(request);
            InputStream source = request.getInputStream();
            byte[] buffer = source.readNBytes(MAX_BODY_BYTES + 1);
            this.complete = buffer.length <= MAX_BODY_BYTES;
            this.held = complete ? buffer : new byte[0];
            this.replay = complete
                ? new ByteArrayInputStream(buffer)
                : new SequenceInputStream(new ByteArrayInputStream(buffer), source);
        }

        byte[] held() {
            return held;
        }

        boolean complete() {
            return complete;
        }

        @Override
        public ServletInputStream getInputStream() {
            InputStream source = replay;
            return new ServletInputStream() {
                @Override
                public int read() throws IOException {
                    return source.read();
                }

                @Override
                public int read(byte[] buffer, int offset, int length) throws IOException {
                    return source.read(buffer, offset, length);
                }

                @Override
                public boolean isFinished() {
                    try {
                        return source.available() == 0;
                    } catch (IOException failed) {
                        return true;
                    }
                }

                @Override
                public boolean isReady() {
                    return true;
                }

                @Override
                public void setReadListener(ReadListener listener) {
                    throw new UnsupportedOperationException("reproit buffers synchronously");
                }
            };
        }

        @Override
        public java.io.BufferedReader getReader() {
            String encoding = getCharacterEncoding();
            var charset = encoding != null
                ? java.nio.charset.Charset.forName(encoding)
                : StandardCharsets.UTF_8;
            return new java.io.BufferedReader(new java.io.InputStreamReader(replay, charset));
        }
    }

    /**
     * Holds the response body (bounded) until the trace can finish, then sets
     * `x-reproit-events` (scan) or records to the Capture, and releases the
     * held bytes. Over the cap the trace finishes without output and the
     * remaining bytes stream straight through.
     */
    private final class HeldResponse extends HttpServletResponseWrapper {
        private final HttpServletResponse target;
        private final BackendTrace trace;
        private final Capture record;
        private final ByteArrayOutputStream held = new ByteArrayOutputStream();
        private boolean released = false;
        private boolean complete = true;
        private PrintWriter writer;
        private ServletOutputStream stream;

        HeldResponse(HttpServletResponse target, BackendTrace trace, Capture record) {
            super(target);
            this.target = target;
            this.trace = trace;
            this.record = record;
        }

        void release(int status, boolean outputKnown) throws IOException {
            if (released) return;
            released = true;
            if (writer != null) writer.flush();
            try {
                if (!trace.finished()) {
                    Object output = outputKnown && complete
                        ? decodeJson(held.toByteArray(), getContentType(), true)
                        : null;
                    trace.finish(output, status, status < 500, effectsComplete);
                    if (record == null) {
                        target.setHeader("x-reproit-events", trace.header());
                    } else {
                        record.record(trace);
                    }
                }
            } catch (RuntimeException ignored) {
                // Oversized or over-long traces drop their header; ship anyway.
            }
            if (held.size() > 0) {
                target.getOutputStream().write(held.toByteArray());
                held.reset();
            }
        }

        private void sink(byte[] buffer, int offset, int length) throws IOException {
            if (released) {
                target.getOutputStream().write(buffer, offset, length);
                return;
            }
            held.write(buffer, offset, length);
            if (held.size() > MAX_BODY_BYTES) {
                complete = false;
                release(getStatus(), false);
            }
        }

        @Override
        public ServletOutputStream getOutputStream() {
            if (stream == null) {
                stream = new ServletOutputStream() {
                    @Override
                    public void write(int value) throws IOException {
                        sink(new byte[] {(byte) value}, 0, 1);
                    }

                    @Override
                    public void write(byte[] buffer, int offset, int length) throws IOException {
                        sink(buffer, offset, length);
                    }

                    @Override
                    public boolean isReady() {
                        return true;
                    }

                    @Override
                    public void setWriteListener(WriteListener listener) {
                        throw new UnsupportedOperationException("reproit buffers synchronously");
                    }
                };
            }
            return stream;
        }

        @Override
        public PrintWriter getWriter() {
            if (writer == null) {
                String encoding = getCharacterEncoding();
                var charset = encoding != null
                    ? java.nio.charset.Charset.forName(encoding)
                    : StandardCharsets.ISO_8859_1;
                writer = new PrintWriter(new OutputStreamWriter(getOutputStream(), charset));
            }
            return writer;
        }

        @Override
        public void sendError(int status) throws IOException {
            release(status, false);
            super.sendError(status);
        }

        @Override
        public void sendError(int status, String message) throws IOException {
            release(status, false);
            super.sendError(status, message);
        }

        @Override
        public void sendRedirect(String location) throws IOException {
            release(HttpServletResponse.SC_FOUND, false);
            super.sendRedirect(location);
        }

        @Override
        public void flushBuffer() throws IOException {
            release(getStatus(), false);
            super.flushBuffer();
        }
    }
}
