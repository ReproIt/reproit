# Hermetic replay mode for reproit-backend-rb.
#
# When `REPROIT_REPLAY` names a `reproit-backend-capture` payload, the same
# Net::HTTP hook and database helper that record exchanges at capture time
# SERVE them instead, so the application re-executes against exactly what
# production saw with no live dependency.
#
# Determinism is a contract here, not a similarity score. Matching is strict:
# the next unconsumed exchange of the same protocol, same method and path,
# body modulo `$reproit` redaction placeholders. The first unmatched call is
# a DIVERGENCE: it writes the structured `REPROIT:DIVERGENCE ` line to stderr
# and the call fails with status 599 (HTTP) or a raised error (database),
# never a fuzzy match.
#
# The envelope pins replay determinism: `TZ`, the wall clock, and
# `replay_rng` yields the seeded stream. Honesty note: the seed makes REPLAY
# runs deterministic; it does not reproduce the randomness the app drew in
# production.

require "json"
require "uri"

module ReproitBackendRb
  module Replay
    DIVERGENCE_MARKER = "REPROIT:DIVERGENCE "

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

      # Strict next-unconsumed match. Returns the exchange or nil (divergence).
      def match(protocol, probe)
        found = @lock.synchronize do
          entry = @entries.find do |candidate|
            !candidate["consumed"] && candidate["exchange"]["protocol"] == protocol
          end
          # Strict ordering within a protocol: the first unconsumed exchange
          # is the only candidate; skipping it would be a fuzzy match.
          if entry && Replay.request_matches?(protocol, entry["exchange"]["request"] || {}, probe)
            entry["consumed"] = true
            entry["exchange"]
          end
        end
        return found if found
        diverge(protocol, probe)
        nil
      end

      def diverge(protocol, probe)
        report = @lock.synchronize do
          @diverged = true
          expected = @entries.find do |candidate|
            !candidate["consumed"] && candidate["exchange"]["protocol"] == protocol
          end
          {
            "protocol" => protocol,
            "got" => probe,
            "expected" => expected.nil? ? nil : expected["exchange"]["request"],
            "consumed" => @entries.count { |candidate| candidate["consumed"] },
            "total" => @entries.length,
          }
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
      { "status" => response["status"] || 200, "headers" => headers, "body" => text }
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
