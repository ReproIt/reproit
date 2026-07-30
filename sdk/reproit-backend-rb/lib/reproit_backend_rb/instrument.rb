# Outbound-exchange capture and hermetic replay for reproit-backend-rb.
#
# `Instrument.install` prepends a module to `Net::HTTP#request`, so every
# dependency call made while a request trace is ambient is recorded on that
# trace as an `effect` event carrying an `exchange`: the request the app sent
# and the response the dependency returned. Because virtually every Ruby HTTP
# client (Net::HTTP itself, HTTParty, Faraday's default adapter, Octokit)
# bottoms out in `Net::HTTP#request`, that one hook is automatic for the
# common case. Clients that open raw sockets or use libcurl bindings are
# invisible to it; those record through `Instrument.db`-style explicit calls
# or not at all, which is stated rather than papered over.
#
# With `REPROIT_REPLAY` naming a capture payload the SAME hook serves the
# recorded exchanges: no socket is opened, so replay needs no live
# dependency. Every capture path fails closed the other way: an
# instrumentation defect must never break the host application's request.

require "net/http"

require_relative "exchange"
require_relative "replay"

module ReproitBackendRb
  module Instrument
    TRACE_KEY = :reproit_backend_rb_trace

    @lock = Mutex.new
    @installed = false
    @session = nil
    @session_loaded = false
    @stats = { captured_exchanges: 0, truncated_bodies: 0, failed_captures: 0 }

    module_function

    # The ambient trace for the current thread, or nil. Framework adapters
    # scope it around the handler; `Instrument.db` and the Net::HTTP hook
    # read it. Thread-local (fiber-local in Ruby), which is the scope a Rack
    # handler runs in.
    def current_trace
      trace = Thread.current[TRACE_KEY]
      trace if trace && !trace.finished?
    end

    def with_trace(trace)
      previous = Thread.current[TRACE_KEY]
      Thread.current[TRACE_KEY] = trace
      yield
    ensure
      Thread.current[TRACE_KEY] = previous
    end

    # Load the replay session once. Also pins the process envelope (TZ), so
    # calling `install` from the entry point pins it before any time-zone
    # sensitive code runs.
    def session
      @lock.synchronize do
        unless @session_loaded
          @session_loaded = true
          path = ENV["REPROIT_REPLAY"].to_s
          unless path.strip.empty?
            @session = Replay::Session.load(path)
            Replay.pin_envelope(@session.envelope)
          end
        end
        @session
      end
    end

    def replaying?
      !session.nil?
    end

    # The capture's seeded stream, or nil outside replay mode. Documented
    # honestly: this pins REPLAY determinism, not the app's original draws.
    def replay_rng
      handle = session
      handle.nil? ? nil : Replay.rng_for(handle.envelope)
    end

    def stats
      @lock.synchronize { @stats.dup }
    end

    def count(key)
      @lock.synchronize { @stats[key] += 1 }
    end

    # Install the Net::HTTP hook once, process-wide. Idempotent.
    def install
      @lock.synchronize do
        return false if @installed
        Net::HTTP.prepend(NetHttpHook)
        @installed = true
      end
      session
      true
    end

    def installed?
      @lock.synchronize { @installed }
    end

    # Record one exchange on the ambient trace. Shared by the HTTP hook and
    # the database helper; never raises into the host.
    def record(kind, resource, key, exchange)
      trace = current_trace
      return if trace.nil?
      trace.effect(kind, resource: resource, key: key, exchange: exchange)
      count(:captured_exchanges)
    rescue StandardError
      count(:failed_captures)
      nil
    end

    # Generic database boundary: run `block` and record the statement with
    # its result, or serve the recorded result in replay mode without
    # touching a driver. The block is never called while replaying, which is
    # what makes a replay run valid with the database stopped.
    #
    # `block` returns a hash shaped { "rows" => [...], "command" => ..,
    # "rowCount" => .. }; anything else records as a bare row count.
    def db(text, values = nil)
      handle = session
      probe = { "text" => text.to_s }
      probe["values"] = values if values.is_a?(Array) && !values.empty?
      unless handle.nil?
        recorded = handle.match("db", probe)
        raise DivergenceError, "reproit: database call diverged from the capture" if recorded.nil?
        outcome = recorded["response"] || {}
        if outcome["error"]
          raise RecordedError, outcome["error"]["message"].to_s
        end
        return {
          "command" => outcome["command"],
          "rowCount" => outcome["rowCount"] || 0,
          "rows" => outcome["rows"].is_a?(Array) ? outcome["rows"] : [],
        }
      end
      begin
        result = yield
      rescue StandardError => error
        record(
          Exchange.statement_effect_kind(text), "db", text.to_s[0, 256],
          Exchange.db(text, values, Exchange.db_error(error))
        )
        raise
      end
      record(
        Exchange.statement_effect_kind(text), "db", text.to_s[0, 256],
        Exchange.db(text, values, Exchange.db_outcome(result))
      )
      result
    end

    # Raised when a replayed call has no matching recorded exchange.
    class DivergenceError < StandardError; end

    # Raised in replay mode when the capture recorded a driver error.
    class RecordedError < StandardError; end

    # Synthesize a Net::HTTPResponse from a served exchange. No socket is
    # involved, so the caller's code path is identical to a live response.
    def synthesize_response(served)
      code = served["status"].to_i
      klass = Net::HTTPResponse::CODE_TO_OBJ[code.to_s] || Net::HTTPResponse
      message = code == 599 ? "Reproit Diverged" : "OK"
      response = klass.new("1.1", code.to_s, message)
      served["headers"].each { |name, value| response[name.to_s] = value.to_s }
      response.instance_variable_set(:@body, served["body"])
      response.instance_variable_set(:@read, true)
      response
    end

    # Prepended to Net::HTTP so one hook covers every client built on it.
    module NetHttpHook
      def request(request_object, body = nil, &block)
        session = Instrument.session
        return Instrument.__replay_request(self, request_object, body) unless session.nil?
        trace = Instrument.current_trace
        return super unless trace
        response = super
        begin
          Instrument.__record_request(self, request_object, body, response)
        rescue StandardError
          Instrument.count(:failed_captures)
        end
        response
      end

      private

      # `Net::HTTP.get_response` and friends open the socket in `start`,
      # BEFORE `request` runs, so hooking `request` alone would still resolve
      # DNS and connect. In replay mode the connection is skipped entirely:
      # the session serves every response, so no socket is ever needed and a
      # replay run is valid with the network denied.
      def connect
        return if Instrument.replaying?
        super
      end
    end

    def absolute_url(http, request_object)
      scheme = http.respond_to?(:use_ssl?) && http.use_ssl? ? "https" : "http"
      port = http.port
      default = scheme == "https" ? 443 : 80
      authority = port == default ? http.address.to_s : "#{http.address}:#{port}"
      "#{scheme}://#{authority}#{request_object.path}"
    rescue StandardError
      request_object.path.to_s
    end

    def request_headers(request_object)
      headers = {}
      request_object.each_header { |name, value| headers[name.to_s] = value }
      headers
    end

    def response_headers(response)
      headers = {}
      response.each_header { |name, value| headers[name.to_s] = value }
      headers
    end

    # Internal: record one live Net::HTTP exchange.
    def __record_request(http, request_object, body, response)
      request_body = body || request_object.body
      # A streaming response consumed by a caller block leaves `body` nil;
      # the exchange is recorded without content rather than half-guessed.
      response_body = begin
        response.body
      rescue StandardError
        nil
      end
      exchange = Exchange.http(
        {
          method: request_object.method.to_s.upcase,
          url: absolute_url(http, request_object),
          headers: request_headers(request_object),
          body: request_body,
          content_type: request_object["content-type"].to_s,
        },
        {
          status: response.code.to_i,
          headers: response_headers(response),
          body: response_body,
          content_type: response["content-type"].to_s,
        }
      )
      record("call", http.address.to_s, "#{request_object.method} #{request_object.path}", exchange)
    end

    # Internal: serve one recorded exchange, opening no socket.
    def __replay_request(http, request_object, body)
      handle = session
      request_body = body || request_object.body
      probe = {
        "method" => request_object.method.to_s.upcase,
        "url" => absolute_url(http, request_object),
      }
      unless request_body.nil? || request_body.to_s.empty?
        probe["body"] = Replay.try_json(request_body.to_s, request_object["content-type"].to_s)
      end
      synthesize_response(Replay.serve_http(handle, probe))
    end
  end
end
