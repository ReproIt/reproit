# Production capture mode: config-gated upload of complete failed operation
# traces to the Repro It Cloud ingest endpoint
# (`/v1/capture-batches`).
#
# Ruby port of sdk/reproit-backend-rs/src/capture.rs. Scan-time tracing stays
# untouched: this module only adds a place to hand a finished BackendTrace when
# no `x-reproit-trace` header exists. A stable 5xx or marked agent oracle,
# complete effects, and a pre-operation replay seed are required before queueing.
#
# Everything is bounded and capture failure is invisible to the host app: a
# fixed-depth queue drops oldest on overflow, batches and retries are capped,
# uploads run on one background thread via stdlib net/http, and `record` never
# blocks or raises.

require "net/http"
require "rbconfig"
require "securerandom"
require "uri"

require_relative "trace"

module ReproitBackendRb
  # Payload format identifier of the replayable capture object attached to the
  # finding context (`context.reproitCapture`).
  CAPTURE_FORMAT = "reproit-backend-capture"
  CAPTURE_VERSION = 1
  # Version stamped when any event carries a captured dependency `exchange`
  # or an envelope stamp. Older readers reject it with a named version error
  # instead of silently evaluating a payload whose replay semantics they do
  # not understand.
  CAPTURE_VERSION_EXCHANGES = 2
  # First-class registry oracle id for an operation that returned HTTP 5xx.
  SERVER_ERROR_ORACLE = "backend-server-error"
  # Agent oracle vocabulary (registry ids, lowest confidence tier): authored
  # assertions an LLM/agent operation marks on its own trace via
  # `trace.oracle(id, detail)`. A marked operation is always captured and its
  # failure observation carries the marked id instead of the 5xx default.
  AGENT_RESPONSE_ORACLE = "agent-response-content"
  AGENT_GUARDRAIL_ORACLE = "agent-guardrail-violation"
  AGENT_LOOP_BOUND_ORACLE = "agent-loop-bound-exceeded"
  AGENT_ORACLES = [
    AGENT_RESPONSE_ORACLE,
    AGENT_GUARDRAIL_ORACLE,
    AGENT_LOOP_BOUND_ORACLE,
  ].freeze
  # The effect resource that carries an oracle marker on the trace. A marker
  # is an `emit` effect so the scan-time wire shape stays inside the existing
  # event vocabulary.
  ORACLE_MARKER_RESOURCE = "reproit-oracle"

  # First agent oracle marked on a finished trace's events, or nil.
  def self.marked_oracle(events)
    (events || []).each do |event|
      next unless event.is_a?(Hash) && event["kind"] == "effect"
      next unless event["resource"] == ORACLE_MARKER_RESOURCE
      return event["key"] if AGENT_ORACLES.include?(event["key"])
    end
    nil
  end

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

  # Where and when the capture happened, and a seed that makes REPLAY runs
  # deterministic. Honesty note: the seed does not reproduce the randomness
  # the app drew in production; it pins the replay's.
  def self.determinism_envelope(observed_at_ms = nil, replay_seed = nil)
    envelope = {
      "observedAtMs" =>
        observed_at_ms.is_a?(Integer) ? observed_at_ms : (Time.now.to_f * 1000).to_i,
      "tz" => Time.now.zone.to_s,
      "runtime" => "ruby #{RUBY_VERSION}",
      "os" => RbConfig::CONFIG["host_os"].to_s,
      "arch" => RbConfig::CONFIG["host_cpu"].to_s,
      "replaySeed" => replay_seed&.match?(/\A[0-9a-f]{16}\z/) ? replay_seed : SecureRandom.hex(8),
    }
    digest = ENV["REPROIT_IMAGE_DIGEST"]
    envelope["imageDigest"] = digest if valid_token?(digest)
    envelope
  end

  # Version 2 the moment any event carries an exchange or an envelope stamp.
  def self.payload_version(events)
    stamped = events.any? do |event|
      event.is_a?(Hash) && (event["exchange"] || event.key?("at") || event.key?("monoNs"))
    end
    stamped ? CAPTURE_VERSION_EXCHANGES : CAPTURE_VERSION
  end

  # The replayable capture object (`reproit debug replay-capture` input).
  # Trailing effect events are dropped first when the payload exceeds the
  # context budget; a payload that stays oversized with only start/return
  # left is omitted entirely (nil). Returns [payload, dropped].
  def self.capture_payload(operation, envelope = nil)
    events = operation["events"].dup
    oracle = marked_oracle(events) || SERVER_ERROR_ORACLE
    dropped = 0
    loop do
      payload = {
        "format" => CAPTURE_FORMAT,
        "version" => payload_version(events),
        "operation" => operation["operation"],
        "oracle" => oracle,
        "events" => events,
      }
      payload["envelope"] = envelope unless envelope.nil?
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
    def self.create(endpoint:, api_key:, app_id:, build: nil, commit: nil,
                    healthy_sample_per_mille: 0, flush_interval_ms: 3000,
                    request_timeout_ms: 5000, retry_limit: 2)
      return nil unless endpoint.is_a?(String) && !endpoint.strip.empty?
      return nil unless api_key.is_a?(String) && !api_key.strip.empty?
      return nil unless ReproitBackendRb.valid_token?(app_id)
      return nil if !build.nil? && !ReproitBackendRb.valid_token?(build)
      return nil if !commit.nil? && !ReproitBackendRb.valid_token?(commit)
      begin
        new(
          endpoint, api_key, app_id, build, resolve_commit(commit),
          [0, Integer(healthy_sample_per_mille)].max,
          [MIN_FLUSH_INTERVAL_MS, Integer(flush_interval_ms)].max,
          Integer(request_timeout_ms),
          [MAX_RETRY_LIMIT, [0, Integer(retry_limit)].max].min
        )
      rescue StandardError
        nil
      end
    end

    # Code identity for the capture, in priority order: explicit config, then
    # the common CI and platform environment. Never shells out to git.
    def self.resolve_commit(commit, env = ENV)
      [commit, env["REPROIT_COMMIT"], env["GITHUB_SHA"]].each do |candidate|
        return candidate if ReproitBackendRb.valid_token?(candidate)
      end
      nil
    end

    def initialize(endpoint, api_key, app_id, build, commit, healthy_sample_per_mille,
                   flush_interval_ms, request_timeout_ms, retry_limit)
      @endpoint = URI.parse(endpoint)
      @api_key = api_key
      @app_id = app_id
      @build = build
      @commit = commit
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
        # Capture-mode traces stamp per-event wall-clock and monotonic
        # offsets (the determinism envelope); scan-time traces never do.
        "capture_envelope" => true,
        "replay_seed" => SecureRandom.hex(8),
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
      status = returned["status"]
      status = nil unless status.is_a?(Integer) && status >= 0 && status <= 0xFFFF
      return unless portable_operation?(events, returned, status)
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
            while @queue.length < 1 && !@flush_now
              remaining = deadline - Process.clock_gettime(Process::CLOCK_MONOTONIC)
              break if remaining <= 0
              @signal.wait(@lock, remaining)
            end
            @flush_now = false
            take = [@queue.length, 1].min
            @sending = true
            return @queue.shift(take)
          end
          @flush_now = false
          @signal.wait(@lock)
        end
      end
    end

    # Build one source-neutral capture-batch-v1 payload.
    def build_batch(operations)
      unless operations.length == 1
        raise ArgumentError, "a causal capture batch must contain exactly one operation"
      end
      operation = operations[0]
      seq = @lock.synchronize { @batch_seq += 1 }
      batch_id = format("cb-ruby-%d-%d", (Time.now.to_f * 1000).to_i, seq)
      first = operation["events"][0] || {}
      trace_id = first["traceId"]
      events = []
      parent = nil
      # Real monotonic offsets from the trace's envelope stamps; the ordinal
      # fallback only applies to traces recorded without capture mode.
      add = lambda do |event, source = nil|
        sequence = events.length + 1
        event_id = "evt_backend-ruby_#{sequence}"
        mono = source.is_a?(Hash) ? source["monoNs"] : nil
        item = {
          "id" => event_id,
          "sequence" => sequence,
          "monotonicNs" => mono.is_a?(Integer) ? mono : sequence,
          "causalParentIds" => parent.nil? ? [] : [parent],
          "event" => event,
        }
        item["traceId"] = trace_id unless trace_id.nil?
        events << item
        parent = event_id
      end
      add.call({ "kind" => "operation-start", "name" => operation["operation"] }, first)
      input = first["input"]
      captured_input = {
        "representation" => "replayable",
        "value" => input,
        "redaction" => "redacted-at-source",
      }
      add.call({
        "kind" => "trigger",
        "trigger" => "http-request",
        "subject" => operation["operation"],
        "value" => captured_input,
      }, first)
      # Determinism envelope: where and when the capture happened, and a seed
      # that makes REPLAY runs deterministic. Honesty note: the seed does not
      # reproduce the app's original randomness; it pins the replay's.
      add.call({
        "kind" => "checkpoint",
        "name" => "determinism-envelope",
        "attributes" => envelope_attributes(first),
      }, first)
      operation["events"].each do |source|
        next unless source["kind"] == "effect"
        effect = source["effect"] || "backend-effect"
        subject = source["resource"] || source["service"] || operation["operation"]
        value = if source["exchange"].is_a?(Hash)
                  {
            "representation" => "replayable",
            "value" => source,
            "redaction" => "redacted-at-source",
                  }
                else
                  {
                    "representation" => "structural",
                    "shape" => { "effect" => effect, "subject" => subject },
                  }
                end
        causal = if effect == "call"
                   {
                     "kind" => "dependency",
                     "system" => "service",
                     "operation" => "call",
                     "subject" => subject,
                     "value" => value,
                   }
                 elsif %w[read write delete].include?(effect)
                   {
                     "kind" => "state-access",
                     "state" => "database",
                     "operation" => effect,
                     "subject" => subject,
                     "value" => value,
                   }
                 else
                   {
                     "kind" => "effect",
                     "effect" => effect,
                     "subject" => subject,
                     "value" => value,
                   }
                 end
        add.call(causal, source)
      end
      returned = operation["events"].reverse.find { |event| event["kind"] == "return" } || {}
      # Nest the raw return event exactly like the raw effect events, so the
      # batch can be projected back to a replayable backend capture. The
      # subject names the carrier: `backend_capture_from_batch` in
      # reproit-protocol keys the inversion on "operation-return".
      unless returned.empty?
        add.call({
          "kind" => "effect",
          "effect" => "operation-return",
          "subject" => "operation-return",
          "value" => {
            "representation" => "replayable",
            "value" => returned,
            "redaction" => "redacted-at-source",
          },
        }, returned)
      end
      add.call({
        "kind" => "operation-end",
        "name" => operation["operation"],
        "outcome" => returned["success"] == true ? "succeeded" : "failed",
      }, returned)
      status = operation["status"]
      marked = ReproitBackendRb.marked_oracle(operation["events"])
      if !marked.nil? || (!status.nil? && status >= 500)
        oracle = marked || SERVER_ERROR_ORACLE
        message = if marked.nil?
                    format(
                      "backend operation %s returned HTTP %d", operation["operation"], status
                    )
                  else
                    format("agent oracle %s fired on %s", oracle, operation["operation"])
                  end
        add.call({
          "kind" => "observation",
          "failure" => {
            # A marked agent oracle is an authored assertion (a declared
            # contract the trace itself violated); a bare 5xx stays the
            # runtime exception it always was.
            "observation" => marked.nil? ? "exception" : "contract-violation",
            "authority" => "runtime-diagnosis",
            "summary" => message,
            "signature" => oracle + ":" + operation["operation"],
            "observationPoint" => operation["operation"],
            "artifactIds" => [],
          },
        })
      end
      batch = {
        "version" => 1,
        "batchId" => batch_id,
        "projectId" => @app_id,
        "sessionId" => trace_id || batch_id,
        "emitter" => {
          "id" => "backend-ruby",
          "kind" => "runtime-sdk",
          "component" => "backend",
          "runtime" => "ruby",
        },
        "observedAt" => (Time.now.to_f * 1000).to_i.to_s,
        "policy" => {
          "consent" => "application-telemetry",
          "retentionClass" => "standard",
        },
        "capabilities" => capabilities(operation),
        "events" => events,
        "artifacts" => [],
      }
      deployment = {}
      deployment["version"] = @build unless @build.nil?
      deployment["commit"] = @commit unless @commit.nil?
      batch["deployment"] = deployment unless deployment.empty?
      batch
    end

    # `network: complete` is declared ONLY when the instrument layer actually
    # recorded exchanges, so a capsule never claims a capability it lacks.
    def capabilities(operation)
      list = [
        { "capability" => "http", "completeness" => "complete" },
      ]
      has_network = operation["events"].any? do |event|
        event.is_a?(Hash) && event["effect"] == "call" && event["exchange"].is_a?(Hash)
      end
      has_database = operation["events"].any? do |event|
        event.is_a?(Hash) &&
          %w[read write delete].include?(event["effect"]) &&
          event["exchange"].is_a?(Hash)
      end
      if has_network
        list << {
          "capability" => "network",
          "completeness" => "complete",
          "detail" => "outbound dependency exchanges recorded with responses",
        }
      end
      if has_database
        list << { "capability" => "database", "completeness" => "complete" }
      end
      list
    end

    def portable_operation?(events, returned, status)
      missing_oracle = ReproitBackendRb.marked_oracle(events).nil? &&
        (status.nil? || status < 500)
      return false if missing_oracle
      return false unless returned["effectsComplete"] == true
      return false unless events[0]["replaySeed"]&.match?(/\A[0-9a-f]{16}\z/)
      events.all? do |event|
        next true unless event.is_a?(Hash) && event["kind"] == "effect"
        next true unless %w[call read write delete].include?(event["effect"])
        event["exchange"].is_a?(Hash)
      end
    end

    def envelope_attributes(first)
      observed_at_ms = first["at"].is_a?(Integer) ? first["at"] : nil
      ReproitBackendRb.determinism_envelope(observed_at_ms, first["replaySeed"])
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
