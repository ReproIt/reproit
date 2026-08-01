/*
 * Delegating java.net.http.HttpClient: the library-layer outbound boundary.
 *
 * `ReproitHttpClient.wrap(client)` returns an HttpClient that behaves exactly
 * like the wrapped one, and additionally records every exchange made while a
 * request trace is ambient: request line, headers and body, response status,
 * headers and body, bounded like every other SDK (8 KiB inline budget, 32
 * name-sorted headers, digest over every byte past the budget). Streaming
 * responses (SSE via HttpResponse.BodyHandlers ofLines / ofInputStream / any
 * subscriber the app brings) are observed through a TEE subscriber, never a
 * drain: chunk boundaries are recorded as the app consumes the body and the
 * exchange lands at EOF; a body the app abandons records nothing, exactly
 * like a response nobody reads.
 *
 * With `REPROIT_REPLAY` set the same client SERVES the recorded exchanges:
 * no socket is opened, the caller's BodyHandler receives the recorded body
 * chunk for chunk (the recorded stream shape), and an unmatched call answers
 * 599 with the structured divergence marker on stderr, never a guess.
 *
 * Named no-weaving gap: only clients wrapped here are visible; a bare
 * HttpClient the app builds itself records nothing and, at replay, would try
 * the real network. HTTP/2 push promises pass through unrecorded.
 */
package dev.reproit.backend;

import java.io.IOException;
import java.net.Authenticator;
import java.net.CookieHandler;
import java.net.ProxySelector;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpHeaders;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CompletionStage;
import java.util.concurrent.Executor;
import java.util.concurrent.Flow;
import javax.net.ssl.SSLContext;
import javax.net.ssl.SSLParameters;
import javax.net.ssl.SSLSession;

public final class ReproitHttpClient extends HttpClient {
    private final HttpClient delegate;

    private ReproitHttpClient(HttpClient delegate) {
        this.delegate = delegate;
    }

    /** Wrap a client. Idempotent: wrapping a wrapped client returns it. */
    public static HttpClient wrap(HttpClient client) {
        if (client instanceof ReproitHttpClient wrapped) return wrapped;
        return new ReproitHttpClient(client);
    }

    @Override
    public <T> HttpResponse<T> send(
            HttpRequest request, HttpResponse.BodyHandler<T> handler)
            throws IOException, InterruptedException {
        Replay session = Instrument.session();
        if (session != null) return serve(session, request, handler);
        BackendTrace trace = Instrument.ambient();
        if (trace == null) return delegate.send(request, handler);
        return delegate.send(request, recording(trace, request, handler));
    }

    @Override
    public <T> CompletableFuture<HttpResponse<T>> sendAsync(
            HttpRequest request, HttpResponse.BodyHandler<T> handler) {
        Replay session = Instrument.session();
        if (session != null) {
            try {
                return CompletableFuture.completedFuture(serve(session, request, handler));
            } catch (RuntimeException failure) {
                return CompletableFuture.failedFuture(failure);
            }
        }
        BackendTrace trace = Instrument.ambient();
        if (trace == null) return delegate.sendAsync(request, handler);
        return delegate.sendAsync(request, recording(trace, request, handler));
    }

    @Override
    public <T> CompletableFuture<HttpResponse<T>> sendAsync(
            HttpRequest request,
            HttpResponse.BodyHandler<T> handler,
            HttpResponse.PushPromiseHandler<T> pushHandler) {
        Replay session = Instrument.session();
        // Push promises are a named gap: never recorded, so replay ignores
        // the push handler and serves the main exchange only.
        if (session != null) return sendAsync(request, handler);
        BackendTrace trace = Instrument.ambient();
        if (trace == null) return delegate.sendAsync(request, handler, pushHandler);
        return delegate.sendAsync(request, recording(trace, request, handler), pushHandler);
    }

    // One request's identity, read before send so a mutated builder cannot
    // skew the record.
    private record RequestMeta(
        String method, URI uri, Map<String, String> headers, byte[] body, String contentType) {}

    private static RequestMeta metaOf(HttpRequest request) {
        Map<String, String> headers = new LinkedHashMap<>();
        request.headers().map().forEach((name, values) -> {
            if (!values.isEmpty()) headers.put(name, values.get(0));
        });
        byte[] body = request.bodyPublisher()
            .map(ReproitHttpClient::drainPublisher)
            .orElse(new byte[0]);
        return new RequestMeta(
            request.method(),
            request.uri(),
            headers,
            body,
            request.headers().firstValue("content-type").orElse(""));
    }

    /** The capture-mode tee: the caller's handler, observed chunk by chunk. */
    private <T> HttpResponse.BodyHandler<T> recording(
            BackendTrace trace, HttpRequest request, HttpResponse.BodyHandler<T> handler) {
        RequestMeta meta;
        try {
            meta = metaOf(request);
        } catch (RuntimeException unrecordable) {
            Instrument.countFailedCapture();
            return handler;
        }
        return info -> new TeeSubscriber<>(handler.apply(info), collector -> {
            try {
                String contentType = info.headers()
                    .firstValue("content-type").orElse("");
                trace.effect("call", new BackendTrace.Effect()
                    .resource(meta.uri().getHost())
                    .key(meta.method() + " " + Replay.pathAndQuery(meta.uri().toString()))
                    .exchange(Exchange.http(
                        meta.method(),
                        meta.uri().toString(),
                        meta.headers(),
                        meta.body(),
                        meta.contentType(),
                        info.statusCode(),
                        firstValues(info.headers()),
                        collector.body(contentType),
                        collector.stream(contentType.contains("text/event-stream")))));
                Instrument.countCapturedExchange();
            } catch (RuntimeException ignored) {
                // The trace may have finished or overflowed; the host call goes on.
                Instrument.countFailedCapture();
            }
        });
    }

    private static Map<String, String> firstValues(HttpHeaders headers) {
        Map<String, String> out = new LinkedHashMap<>();
        headers.map().forEach((name, values) -> {
            if (!values.isEmpty()) out.put(name, values.get(0));
        });
        return out;
    }

    /**
     * Tee one response body: every chunk is forwarded to the downstream
     * subscriber untouched and observed by the collector. The exchange is
     * recorded at onComplete (EOF as the app saw it); onError and an
     * abandoned body record nothing.
     */
    private static final class TeeSubscriber<T> implements HttpResponse.BodySubscriber<T> {
        private final HttpResponse.BodySubscriber<T> downstream;
        private final java.util.function.Consumer<Exchange.BodyCollector> record;
        private final Exchange.BodyCollector collector = new Exchange.BodyCollector();
        private boolean done = false;

        TeeSubscriber(
                HttpResponse.BodySubscriber<T> downstream,
                java.util.function.Consumer<Exchange.BodyCollector> record) {
            this.downstream = downstream;
            this.record = record;
        }

        @Override
        public void onSubscribe(Flow.Subscription subscription) {
            downstream.onSubscribe(subscription);
        }

        @Override
        public void onNext(List<ByteBuffer> item) {
            try {
                for (ByteBuffer buffer : item) {
                    byte[] chunk = new byte[buffer.remaining()];
                    buffer.duplicate().get(chunk);
                    collector.push(chunk);
                }
            } catch (RuntimeException ignored) {
                Instrument.countFailedCapture();
            }
            downstream.onNext(item);
        }

        @Override
        public void onError(Throwable throwable) {
            done = true;
            downstream.onError(throwable);
        }

        @Override
        public void onComplete() {
            if (!done) {
                done = true;
                record.accept(collector);
            }
            downstream.onComplete();
        }

        @Override
        public CompletionStage<T> getBody() {
            return downstream.getBody();
        }
    }

    /** Serve one recorded exchange through the caller's own BodyHandler. */
    private static <T> HttpResponse<T> serve(
            Replay session, HttpRequest request, HttpResponse.BodyHandler<T> handler) {
        RequestMeta meta = metaOf(request);
        Map<String, Object> probe = new LinkedHashMap<>();
        probe.put("method", meta.method());
        probe.put("url", meta.uri().toString());
        if (meta.body().length > 0) {
            probe.put("body", Replay.tryJson(
                new String(meta.body(), StandardCharsets.UTF_8), meta.contentType()));
        }
        Replay.Served served = session.serveHttp(probe);
        Map<String, List<String>> headerMap = new LinkedHashMap<>();
        served.headers().forEach((name, value) -> headerMap.put(name, List.of(value)));
        HttpHeaders headers = HttpHeaders.of(headerMap, (name, value) -> true);
        HttpResponse.BodySubscriber<T> subscriber = handler.apply(
            new ServedResponseInfo(served.status(), headers));
        subscriber.onSubscribe(new Flow.Subscription() {
            @Override
            public void request(long count) {}

            @Override
            public void cancel() {}
        });
        // The recorded stream shape (SSE / chunked) re-serves chunk for
        // chunk, so a lines or stream consumer observes the boundaries
        // production saw.
        List<byte[]> parts = served.chunks() != null
            ? served.chunks() : List.of(served.body());
        for (byte[] part : parts) subscriber.onNext(List.of(ByteBuffer.wrap(part)));
        subscriber.onComplete();
        T body = subscriber.getBody().toCompletableFuture().join();
        return new ServedResponse<>(request, served.status(), headers, body);
    }

    private record ServedResponseInfo(int statusCode, HttpHeaders headers)
            implements HttpResponse.ResponseInfo {
        @Override
        public Version version() {
            return Version.HTTP_1_1;
        }
    }

    private record ServedResponse<T>(
        HttpRequest request, int statusCode, HttpHeaders headers, T body)
            implements HttpResponse<T> {
        @Override
        public Optional<HttpResponse<T>> previousResponse() {
            return Optional.empty();
        }

        @Override
        public Optional<SSLSession> sslSession() {
            return Optional.empty();
        }

        @Override
        public URI uri() {
            return request.uri();
        }

        @Override
        public Version version() {
            return Version.HTTP_1_1;
        }
    }

    /**
     * Drain a request body publisher synchronously and boundedly. Standard
     * publishers (ofString, ofByteArray, ofFile) are re-subscribable, so the
     * delegate still reads the body itself afterwards.
     */
    static byte[] drainPublisher(HttpRequest.BodyPublisher publisher) {
        var buffer = new java.io.ByteArrayOutputStream();
        var done = new java.util.concurrent.CountDownLatch(1);
        publisher.subscribe(new Flow.Subscriber<ByteBuffer>() {
            @Override
            public void onSubscribe(Flow.Subscription subscription) {
                subscription.request(Long.MAX_VALUE);
            }

            @Override
            public void onNext(ByteBuffer item) {
                byte[] chunk = new byte[item.remaining()];
                item.get(chunk);
                // One byte past the budget is enough to know the size class.
                if (buffer.size() <= Exchange.MAX_EXCHANGE_BODY_BYTES) {
                    buffer.write(chunk, 0, chunk.length);
                }
            }

            @Override
            public void onError(Throwable throwable) {
                done.countDown();
            }

            @Override
            public void onComplete() {
                done.countDown();
            }
        });
        try {
            // A body publisher is materialized in memory here, so this
            // completes immediately; the bound stops a pathological publisher
            // from blocking the host call.
            if (!done.await(5, java.util.concurrent.TimeUnit.SECONDS)) return new byte[0];
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            return new byte[0];
        }
        return buffer.toByteArray();
    }

    // Pure delegation below: the wrapped client's configuration is the
    // configuration.
    @Override
    public Optional<CookieHandler> cookieHandler() {
        return delegate.cookieHandler();
    }

    @Override
    public Optional<Duration> connectTimeout() {
        return delegate.connectTimeout();
    }

    @Override
    public Redirect followRedirects() {
        return delegate.followRedirects();
    }

    @Override
    public Optional<ProxySelector> proxy() {
        return delegate.proxy();
    }

    @Override
    public SSLContext sslContext() {
        return delegate.sslContext();
    }

    @Override
    public SSLParameters sslParameters() {
        return delegate.sslParameters();
    }

    @Override
    public Optional<Authenticator> authenticator() {
        return delegate.authenticator();
    }

    @Override
    public Version version() {
        return delegate.version();
    }

    @Override
    public Optional<Executor> executor() {
        return delegate.executor();
    }
}
