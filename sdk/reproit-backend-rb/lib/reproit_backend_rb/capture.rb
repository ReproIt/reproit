# Production capture mode: config-gated self-sampling upload of finished
# operation traces to the Reproit Cloud ingest endpoint (`/v1/events`).
#
# Ruby port of sdk/reproit-backend-rs/src/capture.rs. Scan-time tracing stays
# untouched: this module only adds a place to hand a finished BackendTrace when
# no `x-reproit-trace` header exists. Operations that end in a server error
# (HTTP 5xx) or report `success == false` are always captured; healthy
# operations only under an optional per-mille baseline sample (default 0).
#
# Everything is bounded and capture failure is invisible to the host app: a
# fixed-depth queue drops oldest on overflow, batches and retries are capped,
# uploads run on one background thread via stdlib net/http, and `record` never
# blocks or raises.

require "net/http"
require "uri"

require_relative "trace"

module ReproitBackendRb
  # Payload format identifier of the replayable capture object attached to the
  # finding context (`context.reproitCapture`).
  CAPTURE_FORMAT = "reproit-backend-capture"
  CAPTURE_VERSION = 1
  # First-class registry oracle id for an operation that returned HTTP 5xx.
  SERVER_ERROR_ORACLE = "backend-server-error"

  # Bounds. Queue overflow drops the OLDEST pending operation; an oversized
  # capture payload drops trailing effect events before it drops itself.
  MAX_QUEUE_OPERATIONS = 64
  MAX_BATCH_OPERATIONS = 16
  MAX_CAPTURE_JSON_BYTES = 48 * 1024
  MIN_FLUSH_INTERVAL_MS = 100
  MAX_RETRY_LIMIT = 5

  # The ingest protocol token charset (`validate_token` in reproit-protocol).
  TOKEN_PATTERN = /\A[A-Za-z0-9\-_.:]{1,128}\z/

  def self.valid_token?(value)
    value.is_a?(String) && TOKEN_PATTERN.match?(value)
  end

  # The replayable capture object (`reproit debug replay-capture` input).
  # Trailing effect events are dropped first when the payload exceeds the
  # context budget; a payload that stays oversized with only start/return
  # left is omitted entirely (nil). Returns [payload, dropped].
  def self.capture_payload(operation)
    events = operation["events"].dup
    dropped = 0
    loop do
      payload = {
        "format" => CAPTURE_FORMAT,
        "version" => CAPTURE_VERSION,
        "operation" => operation["operation"],
        "oracle" => SERVER_ERROR_ORACLE,
        "events" => events,
      }
      if canonical_json(payload).bytesize <= MAX_CAPTURE_JSON_BYTES
        return [payload, dropped]
      end
      last_effect = events.rindex { |event| event.is_a?(Hash) && event["kind"] == "effect" }
      return [nil, dropped] if last_effect.nil?
      events.delete_at(last_effect)
      dropped += 1
    end
  end

  # Handle to the capture worker. Thread-safe; one queue, one upload thread.
  class Capture
    # Start capture mode. Returns nil (capture disabled, host unaffected)
    # when the config is unusable: empty endpoint/key or identifiers the
    # ingest protocol would reject.
    def self.create(endpoint:, api_key:, app_id:, build: nil, healthy_sample_per_mille: 0,
                    flush_interval_ms: 3000, request_timeout_ms: 5000, retry_limit: 2)
      return nil unless endpoint.is_a?(String) && !endpoint.strip.empty?
      return nil unless api_key.is_a?(String) && !api_key.strip.empty?
      return nil unless ReproitBackendRb.valid_token?(app_id)
      return nil if !build.nil? && !ReproitBackendRb.valid_token?(build)
      begin
        new(
          endpoint, api_key, app_id, build,
          [0, Integer(healthy_sample_per_mille)].max,
          [MIN_FLUSH_INTERVAL_MS, Integer(flush_interval_ms)].max,
          Integer(request_timeout_ms),
          [MAX_RETRY_LIMIT, [0, Integer(retry_limit)].max].min
        )
      rescue StandardError
        nil
      end
    end

    def initialize(endpoint, api_key, app_id, build, healthy_sample_per_mille,
                   flush_interval_ms, request_timeout_ms, retry_limit)
      @endpoint = URI.parse(endpoint)
      @api_key = api_key
      @app_id = app_id
      @build = build
      @healthy_sample_per_mille = healthy_sample_per_mille
      @flush_interval = flush_interval_ms / 1000.0
      @request_timeout = request_timeout_ms / 1000.0
      @retry_limit = retry_limit
      @lock = Mutex.new
      @signal = ConditionVariable.new
      @queue = []
      @sending = false
      @flush_now = false
      @trace_seq = 0
      @batch_seq = 0
      @stats = {
        captured_operations: 0,
        dropped_operations: 0,
        sent_batches: 0,
        failed_batches: 0,
      }
      worker = Thread.new { run_worker }
      worker.name = "reproit-capture"
      worker.abort_on_exception = false
    end

    # Synthesized trace context for capture-mode operations, replacing the
    # scan-time `x-reproit-trace` header requirement.
    def context
      seq = @lock.synchronize { @trace_seq += 1 }
      {
        "trace_id" => format("cap-%d-%d", (Time.now.to_f * 1000).to_i, seq),
        "actor" => nil,
        "action_index" => 0,
        "build" => @build,
        "config_contract" => nil,
      }
    end

    # Hand a finished trace to the sampler. Unfinished traces are ignored.
    # Never blocks and never fails visibly; overflow drops the oldest
    # queued operation.
    def record(trace)
      events = trace.events
      returned = events.reverse_each.find do |event|
        event.is_a?(Hash) && event["kind"] == "return"
      end
      return if returned.nil?
      success = returned.fetch("success", true)
      status = returned["status"]
      status = nil unless status.is_a?(Integer) && status >= 0 && status <= 0xFFFF
      error = success == false || (!status.nil? && status >= 500)
      return if !error && !sample_healthy?
      operation = events.empty? ? nil : events[0]["operation"]
      return unless operation.is_a?(String)
      captured = { "operation" => operation, "status" => status, "events" => events.dup }
      @lock.synchronize do
        @stats[:captured_operations] += 1
        @queue << captured
        if @queue.length > MAX_QUEUE_OPERATIONS
          @queue.shift
          @stats[:dropped_operations] += 1
        end
        @signal.broadcast
      end
    rescue StandardError
      # Capture must never surface errors into the host app.
      nil
    end

    # Block up to `timeout` seconds until every queued operation has been
    # sent (or dropped). Returns false on timeout. Intended for tests,
    # examples, and graceful shutdown.
    def flush(timeout)
      deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + timeout
      @lock.synchronize do
        @flush_now = true
        @signal.broadcast
        while !@queue.empty? || @sending
          remaining = deadline - Process.clock_gettime(Process::CLOCK_MONOTONIC)
          return false if remaining <= 0
          @signal.wait(@lock, remaining)
        end
        true
      end
    end

    def stats
      @lock.synchronize { @stats.dup }
    end

    # Internal below this point; exposed for the parity tests only.

    def sample_healthy?
      per_mille = @healthy_sample_per_mille
      return false if per_mille <= 0
      return true if per_mille >= 1000
      rand * 1000 < per_mille
    end

    def run_worker
      loop do
        operations = next_batch
        batch = build_batch(operations)
        sent = send_batch(batch)
        @lock.synchronize do
          if sent
            @stats[:sent_batches] += 1
          else
            @stats[:failed_batches] += 1
            @stats[:dropped_operations] += operations.length
          end
          @sending = false
          @signal.broadcast
        end
      rescue StandardError
        # The worker must survive any defect; fail closed and keep draining.
        @lock.synchronize do
          @sending = false
          @signal.broadcast
        end
      end
    end

    # Wait for work, gather up to the batch cap within one flush interval,
    # then drain. `@flush_now` (set by `flush`) cuts the gather short.
    def next_batch
      @lock.synchronize do
        loop do
          if !@queue.empty?
            deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + @flush_interval
            while @queue.length < MAX_BATCH_OPERATIONS && !@flush_now
              remaining = deadline - Process.clock_gettime(Process::CLOCK_MONOTONIC)
              break if remaining <= 0
              @signal.wait(@lock, remaining)
            end
            @flush_now = false
            take = [@queue.length, MAX_BATCH_OPERATIONS].min
            @sending = true
            return @queue.shift(take)
          end
          @flush_now = false
          @signal.wait(@lock)
        end
      end
    end

    # Build one event-batch-v1 payload: every captured event ships as a
    # `backend` frame, and each 5xx operation additionally ships a `finding`
    # frame tagged `backend-server-error` whose context carries the full
    # replayable capture object.
    def build_batch(operations)
      seq = @lock.synchronize { @batch_seq += 1 }
      batch_id = format("cap-%d-%d", (Time.now.to_f * 1000).to_i, seq)
      frames = []
      frame = lambda do |event|
        frames << {
          "runId" => batch_id,
          "sequence" => frames.length + 1,
          "scope" => { "domain" => "shared" },
          "event" => event,
        }
      end
      operations.each do |operation|
        operation["events"].each do |event|
          frame.call({ "kind" => "backend", "evidence" => event })
        end
        status = operation["status"]
        next if status.nil? || status < 500
        signature = "backend:" + operation["operation"]
        message = format(
          "backend operation %s returned HTTP %d", operation["operation"], status
        )
        context = { "capture" => "reproit-backend-rb" }
        context["build"] = { "version" => @build } unless @build.nil?
        payload, dropped = ReproitBackendRb.capture_payload(operation)
        if payload.nil?
          context["captureOmitted"] = true
        else
          context["reproitCapture"] = payload
          context["captureDroppedEffects"] = dropped if dropped > 0
        end
        frame.call({
          "kind" => "finding",
          "signature" => signature,
          "message" => message,
          "identity" => {
            "oracle" => SERVER_ERROR_ORACLE,
            "invariant" => "backend:server-error",
            "kind" => "server-error",
            "message" => message,
            "frame" => "",
            "trigger" => signature,
            "boundary" => signature,
          },
          "path" => [],
          "context" => context,
        })
      end
      batch = {
        "version" => 1,
        "batchId" => batch_id,
        "appId" => @app_id,
        "frames" => frames,
        "evidence" => [],
      }
      batch["deployment"] = { "version" => @build } unless @build.nil?
      batch
    end

    def send_batch(batch)
      body = ReproitBackendRb.canonical_json(batch)
      (@retry_limit + 1).times do |attempt|
        begin
          http = Net::HTTP.new(@endpoint.host, @endpoint.port)
          http.use_ssl = @endpoint.scheme == "https"
          http.open_timeout = @request_timeout
          http.read_timeout = @request_timeout
          http.write_timeout = @request_timeout
          request = Net::HTTP::Post.new(@endpoint.request_uri)
          request["Authorization"] = "Bearer " + @api_key
          request["Content-Type"] = "application/json"
          request.body = body
          response = http.request(request)
          code = response.code.to_i
          return true if code >= 200 && code < 400
          # A definitive client-side rejection cannot improve on retry.
          return false if code >= 400 && code < 500
        rescue StandardError
          nil
        end
        sleep((200 * attempt + 200) / 1000.0) if attempt < @retry_limit
      end
      false
    end
  end
end
