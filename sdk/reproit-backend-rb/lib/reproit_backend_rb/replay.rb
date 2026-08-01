# Hermetic replay mode for reproit-backend-rb.
#
# When `REPROIT_REPLAY` names a `reproit-backend-capture` payload, the same
# Net::HTTP hook and database helper that record exchanges at capture time
# SERVE them instead, so the application re-executes against exactly what
# production saw with no live dependency.
#
# Determinism is a contract here, not a similarity score. Matching is strict
# per-operation ordinals: within one operation (method plus path for HTTP,
# statement text for the database) exchanges are consumed in recorded order,
# so pooled database clients and LLM tool-call loops that interleave
# operations still match exactly. Recorded `$reproit` redaction placeholders
# match any value at their position; nothing else is tolerated. The first
# unmatched call is a DIVERGENCE: it writes the structured
# `REPROIT:DIVERGENCE ` line to stderr (with a `bodyDelta` naming WHERE the
# bodies differ; chat-shaped bodies name the first differing message index)
# and the call fails with status 599 (HTTP) or a raised error (database),
# never a fuzzy match.
#
# The envelope pins replay determinism: `TZ`, the wall clock, `srand` for
# Kernel#rand, and `replay_rng` yields the cross-SDK seeded stream. Honesty
# note: the seed makes REPLAY runs deterministic; it does not reproduce the
# randomness the app drew in production. SecureRandom reads OS entropy
# directly and CANNOT be pinned; that is a named gap, not a downgrade.

require "json"
require "uri"

module ReproitBackendRb
  module Replay
    DIVERGENCE_MARKER = "REPROIT:DIVERGENCE "
    # Sentinel for a body FIELD that is absent, as distinct from a recorded
    # null body (the Node reference's `undefined` vs `null`). A bodyDelta is
    # only computed when both sides actually carry a body field.
    ABSENT = Object.new.freeze

    # Deterministic xorshift64* stream, identical to the Node and Rust SDKs.
    class Rng
      def initialize(state)
        @state = state | 1
      end

      MASK = 0xFFFF_FFFF_FFFF_FFFF

      def next_float
        @state ^= (@state << 13) & MASK
        @state ^= @state >> 7
        @state ^= (@state << 17) & MASK
        mixed = (@state * 0x2545_F491_4F6C_DD1D) & MASK
        (mixed >> 11).to_f / (1 << 53).to_f
      end
    end

    class Session
      attr_reader :envelope

      def self.load(path)
        payload = JSON.parse(File.read(path))
        unless payload["format"] == "reproit-backend-capture"
          raise ArgumentError, "REPROIT_REPLAY file is not a reproit-backend-capture payload"
        end
        version = payload["version"]
        unless version.is_a?(Integer) && version >= 1 && version <= 2
          raise ArgumentError, "unsupported capture version #{version.inspect}"
        end
        new(payload)
      end

      def initialize(payload)
        @envelope = payload["envelope"]
        @entries = (payload["events"] || []).filter_map do |event|
          next unless event.is_a?(Hash) && event["kind"] == "effect" && event["exchange"]
          { "exchange" => event["exchange"], "consumed" => false }
        end
        @diverged = false
        @lock = Mutex.new
      end

      def diverged?
        @lock.synchronize { @diverged }
      end

      # Strict per-operation ordinal match. Returns the exchange or nil
      # (divergence).
      def match(protocol, probe)
        key = Replay.operation_key(protocol, probe)
        found = @lock.synchronize do
          hit = nil
          @entries.each do |candidate|
            next if candidate["consumed"] || candidate["exchange"]["protocol"] != protocol
            request = candidate["exchange"]["request"] || {}
            next unless Replay.operation_key(protocol, request) == key
            if Replay.request_matches?(protocol, request, probe)
              candidate["consumed"] = true
              hit = candidate["exchange"]
            end
            # Strict ordinal within an operation: the next unconsumed
            # exchange of THIS operation is the only candidate; skipping it
            # silently would be a fuzzy match. Other operations' exchanges
            # may interleave (database pooling, tool-call loops), which is
            # why the key filters above.
            break
          end
          hit
        end
        return found if found
        diverge(protocol, probe)
        nil
      end

      def diverge(protocol, probe)
        key = Replay.operation_key(protocol, probe)
        report = @lock.synchronize do
          @diverged = true
          candidates = @entries.reject do |candidate|
            candidate["consumed"] || candidate["exchange"]["protocol"] != protocol
          end
          expected = candidates.find do |candidate|
            Replay.operation_key(protocol, candidate["exchange"]["request"] || {}) == key
          end || candidates.first
          # Field insertion order mirrors the Node reference so the marker
          # line is byte-comparable across SDKs; JSON.generate is compact.
          lines = {
            "protocol" => protocol,
            "got" => probe,
            "expected" => expected.nil? ? nil : expected["exchange"]["request"],
            "consumed" => @entries.count { |candidate| candidate["consumed"] },
            "total" => @entries.length,
          }
          # Prompt drift: when the recorded and live bodies both exist and
          # differ, name WHERE they differ. Chat-shaped bodies (OpenAI or
          # Anthropic messages arrays) name the first differing message
          # index; unknown shapes fall back to the first differing byte.
          unless expected.nil?
            recorded_body = (expected["exchange"]["request"] || {}).fetch("body", ABSENT)
            delta = Replay.body_delta(recorded_body, probe.fetch("body", ABSENT))
            lines["bodyDelta"] = delta unless delta.nil?
          end
          lines
        end
        # Written raw, never through `warn`: Kernel#warn with `uplevel`
        # prepends "file:line: warning: ", which would break the marker
        # prefix the CLI matches on. The line must be byte-identical to the
        # Node and Rust SDKs' so one parser reads every platform.
        $stderr.write(DIVERGENCE_MARKER + JSON.generate(report) + "\n")
        nil
      end
    end

    module_function

    # One operation's identity for ordinal matching: HTTP is method plus
    # path and query, database is the exact statement text.
    def operation_key(protocol, request)
      if protocol == "http"
        request["method"].to_s + " " + path_and_query(request["url"])
      else
        request["text"].to_s
      end
    end

    # The messages array of an OpenAI/Anthropic-shaped chat body, else nil.
    def chat_messages(body)
      body.is_a?(Hash) && body["messages"].is_a?(Array) ? body["messages"] : nil
    end

    def delta_bytes(value)
      raw = value.is_a?(String) ? value : JSON.generate(value)
      raw.dup.force_encoding(Encoding::BINARY)
    end

    # Locate the first difference between a recorded request body and a live
    # one, modulo redaction placeholders. Nil when there is nothing to
    # report (either body missing, or no difference the matcher objects to).
    def body_delta(recorded, live)
      return nil if recorded.equal?(ABSENT) || live.equal?(ABSENT)
      return nil if matches?(recorded, live)
      recorded_messages = chat_messages(recorded)
      live_messages = chat_messages(live)
      if recorded_messages && live_messages
        bound = [recorded_messages.length, live_messages.length].min
        index = (0...bound).find do |i|
          !matches?(recorded_messages[i], live_messages[i])
        end
        # All shared indexes match: the drift is a longer or shorter
        # conversation, and the first differing message is the first
        # unshared one. If lengths also agree the drift is outside
        # `messages`; fall through to bytes.
        if index.nil? && recorded_messages.length != live_messages.length
          index = bound
        end
        unless index.nil?
          return {
            "kind" => "message",
            "firstDifferingMessage" => index,
            "recordedMessages" => recorded_messages.length,
            "liveMessages" => live_messages.length,
          }
        end
      end
      recorded_raw = delta_bytes(recorded)
      live_raw = delta_bytes(live)
      bound = [recorded_raw.bytesize, live_raw.bytesize].min
      offset = (0...bound).find { |i| recorded_raw.getbyte(i) != live_raw.getbyte(i) } || bound
      { "kind" => "byte", "offset" => offset }
    end

    # A recorded value matches a live one when equal, when the recorded side
    # is a `$reproit` redaction placeholder (any value stood there at
    # capture), or when the recorded side is absent. Hashes compare per key.
    def matches?(recorded, live)
      case recorded
      when nil then true
      when Hash
        return true if recorded.key?("$reproit")
        return false unless live.is_a?(Hash)
        recorded.all? { |key, value| matches?(value, live[key]) }
      when Array
        return false unless live.is_a?(Array) && live.length == recorded.length
        recorded.each_with_index.all? { |item, index| matches?(item, live[index]) }
      else recorded == live
      end
    end

    def request_matches?(protocol, recorded, probe)
      if protocol == "http"
        return false unless recorded["method"] == probe["method"]
        return false unless path_and_query(recorded["url"]) == path_and_query(probe["url"])
        # Recorded headers are deliberately not matched: they carry per-run
        # noise (dates, connection management) that would turn every replay
        # into a divergence.
        matches?(recorded["body"], probe["body"])
      else
        return false unless recorded["text"] == probe["text"]
        matches?(recorded["values"], probe["values"])
      end
    end

    def path_and_query(url)
      parsed = URI.parse(url.to_s)
      query = parsed.query.nil? ? "" : "?" + parsed.query
      (parsed.path.nil? || parsed.path.empty? ? "/" : parsed.path) + query
    rescue URI::InvalidURIError
      url.to_s
    end

    # Resolve a live HTTP probe against the session entirely in process. A
    # divergence and a truncated-at-capture body both serve a hard 599 so the
    # application observes an attributable failure instead of a guess.
    def serve_http(session, probe)
      recorded = session.match("http", probe)
      return diverged_599("diverged") if recorded.nil?
      response = recorded["response"] || {}
      if response["truncated"] == true
        # The capture kept identity but not bytes; serving a guessed body
        # would be a silent lie. Fail closed with the named reason.
        session.diverge("http", probe.merge("truncated" => true))
        return diverged_599("truncated-exchange-body")
      end
      headers = (response["headers"] || {}).reject do |name, _|
        %w[content-length transfer-encoding content-encoding].include?(name.to_s.downcase)
      end
      body = response["body"]
      text = case body
             when nil then ""
             when String then body
             else JSON.generate(body)
             end
      served = { "status" => response["status"] || 200, "headers" => headers, "body" => text }
      stream = response["stream"]
      if stream.is_a?(Hash) && stream["chunks"].is_a?(Array)
        if stream["truncated"] == true
          # The capture kept the body but not every chunk boundary; serving
          # a guessed stream shape would be a silent lie. Fail closed with
          # the named reason.
          session.diverge("http", probe.merge("streamBoundariesTruncated" => true))
          return diverged_599("truncated-stream-boundaries")
        end
        served["chunks"] = split_chunks(text, stream["chunks"])
      end
      served
    end

    # Split a replayed body at the recorded chunk boundaries (byte lengths).
    # Redaction can change body byte counts, so lengths are clamped and the
    # last chunk absorbs any remainder: the CHUNK COUNT (the stream shape the
    # app observed) is preserved exactly, the recorded content never padded.
    def split_chunks(body_text, lengths)
      raw = body_text.dup.force_encoding(Encoding::BINARY)
      chunks = []
      offset = 0
      lengths.each_with_index do |length, index|
        last = index == lengths.length - 1
        size = length.is_a?(Integer) && length.positive? ? length : 0
        finish = last ? raw.bytesize : [offset + size, raw.bytesize].min
        chunks << raw.byteslice(offset, finish - offset)
        offset = finish
      end
      chunks
    end

    def diverged_599(reason)
      {
        "status" => 599,
        "headers" => { "content-type" => "application/json" },
        "body" => JSON.generate({ "reproit" => reason }),
      }
    end

    def try_json(text, content_type)
      return text unless content_type.to_s.include?("application/json")
      JSON.parse(text)
    rescue JSON::ParserError
      text
    end

    # Pin process determinism from the capture envelope: the time zone, the
    # wall clock, and the seeded stream.
    #
    # The clock is anchored by prepending a module to Time's singleton class
    # (the Timecop pattern), which is the one safe process-wide hook Ruby
    # offers. It runs ONLY under REPROIT_REPLAY. Like the Node reference this
    # OFFSETS rather than freezes: replayed code sees the capture's instant
    # and still observes elapsed time within the run, so a timeout loop
    # terminates instead of hanging.
    #
    # Honesty note: this makes replay runs repeatable. It does not reproduce
    # the exact instants production observed between events.
    def pin_envelope(envelope)
      return unless envelope.is_a?(Hash)
      tz = envelope["tz"]
      ENV["TZ"] = tz if tz.is_a?(String) && !tz.empty?
      pin_clock(envelope["observedAtMs"])
      pin_rand(envelope["replaySeed"])
    end

    # Seed Kernel#rand (the default Random stream) from the envelope so
    # replayed application draws are repeatable across runs. `replay_rng`
    # additionally exposes the cross-SDK xorshift64* stream. SecureRandom
    # reads OS entropy directly and cannot be pinned: a NAMED gap.
    def pin_rand(seed)
      return unless seed.is_a?(String) && !seed.empty?
      srand(seed[0, 16].ljust(16, "0").to_i(16))
      true
    end

    def pin_clock(observed_at_ms)
      return unless observed_at_ms.is_a?(Numeric)
      offset = (observed_at_ms / 1000.0) - Time.now.to_f
      Time.singleton_class.prepend(clock_module(offset))
      Process.singleton_class.prepend(process_clock_module(offset))
      true
    end

    def clock_module(offset)
      Module.new do
        define_method(:now) { |*args| super(*args) + offset }
        define_method(:reproit_clock_offset) { offset }
      end
    end

    def process_clock_module(offset)
      Module.new do
        define_method(:clock_gettime) do |clock_id, *rest|
          value = super(clock_id, *rest)
          # Only the wall clock is anchored; monotonic clocks must keep
          # advancing from their own epoch or duration math breaks.
          next value unless clock_id == Process::CLOCK_REALTIME
          unit = rest.first
          case unit
          when :millisecond then value + (offset * 1000).round
          when :microsecond then value + (offset * 1_000_000).round
          when :nanosecond then value + (offset * 1_000_000_000).round
          else value + offset
          end
        end
      end
    end

    def rng_for(envelope)
      return nil unless envelope.is_a?(Hash)
      seed = envelope["replaySeed"]
      return nil unless seed.is_a?(String) && !seed.empty?
      Rng.new(seed[0, 16].ljust(16, "0").to_i(16))
    end
  end
end
