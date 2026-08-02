# CI capture mode for reproit-backend-rb: the flaky-CI wedge.
#
# `CI.suite(name)` returns a Minitest-backed `t.call(name) { ... }` whose
# trigger identity is the TEST (suite plus test id), not an inbound HTTP
# request. With `REPROIT_CI_CAPTURE=1` every test runs inside its own trace,
# so the instrumented Net::HTTP hook and db helper (instrument.rb) record
# dependency exchanges and the determinism envelope exactly as production
# capture does; a FAILING test emits a version-2 `reproit-backend-capture`
# capsule to a bounded on-disk spool. With `REPROIT_REPLAY` set the SAME
# wrapper re-runs only the capsule's named test while the SDK serves the
# recorded exchanges in process, and reports the observed result as a
# structured stderr marker for `reproit check`. Without either env the
# wrapper is plain Minitest untouched.
#
# The wire is the existing capture payload: the test identity rides in the
# `operation` field as `test:<suite>#<test>`, and the failed assertion is
# the existing `backend-authored-invariant` registry oracle (a test IS an
# authored invariant). No new protocol fields, no new oracle ids.
#
# Honest limit: replay pins the envelope and the recorded exchanges, which
# is the whole boundary this SDK can see. A race the boundary cannot see
# (scheduling, shared memory) is not reproduced by this capsule; `reproit
# check` reports such runs Inconclusive, never a fake reproduction.
#
# Loaded via `ReproitBackendRb.autoload`, never by requiring the SDK: this
# file pulls in minitest/autorun, which installs an at_exit test runner no
# production host should inherit.

require "digest"
require "fileutils"
require "json"
require "minitest/autorun"

require_relative "trace"
require_relative "capture"
require_relative "instrument"

module ReproitBackendRb
  module CI
    # Test-trigger identity prefix inside the existing `operation` field.
    TEST_TRIGGER_PREFIX = "test:"
    # The registry oracle a failed test capsule carries: an authored
    # invariant (the test's own assertion) was violated. Existing id, not a
    # new one.
    TEST_FAILURE_ORACLE = "backend-authored-invariant"
    # Structured stderr markers `reproit check` parses, like
    # REPROIT:DIVERGENCE.
    RESULT_MARKER = "REPROIT:CI-TEST "
    SPOOL_MARKER = "REPROIT:CI-CAPSULE "

    # Spool bounds. The cap covers the TOTAL bytes on disk; spilled capsules
    # beyond it are dropped and counted (in-process stats plus the on-disk
    # `dropped.count`), never silently.
    DEFAULT_SPOOL_DIR = ".reproit/ci-spool"
    DEFAULT_SPOOL_MAX_BYTES = 16 * 1024 * 1024
    SPOOL_MAX_FLOOR_BYTES = 4 * 1024
    SPOOL_MAX_CEIL_BYTES = 64 * 1024 * 1024
    # Suite and test names share the operation field's 256-code-point bound.
    MAX_NAME = 120
    MAX_ERROR_CHARS = 2048

    @lock = Mutex.new
    @trace_seq = 0
    @suite_seq = 0
    @stats = { spooled_capsules: 0, dropped_capsules: 0, failed_captures: 0 }
    @replay_target = nil
    @replay_target_loaded = false

    module_function

    def stats
      @lock.synchronize { @stats.dup }
    end

    def count(key)
      @lock.synchronize { @stats[key] += 1 }
    end

    def replay_path
      value = ENV["REPROIT_REPLAY"].to_s
      value.empty? ? nil : value
    end

    def mode
      return "replay" unless replay_path.nil?
      return "capture" if ENV["REPROIT_CI_CAPTURE"] == "1"
      "off"
    end

    def bounded_name(value)
      value.to_s.strip[0, MAX_NAME]
    end

    def operation_for(suite_name, test_name)
      TEST_TRIGGER_PREFIX + bounded_name(suite_name) + "#" + bounded_name(test_name)
    end

    def bounded_error(error)
      message = error.respond_to?(:message) ? error.message : error
      message.to_s[0, MAX_ERROR_CHARS]
    end

    # Synthesized trace context: the CI job stands where production stood.
    def ci_context
      seq = @lock.synchronize { @trace_seq += 1 }
      commit = [ENV["REPROIT_COMMIT"], ENV["GITHUB_SHA"]].find do |value|
        ReproitBackendRb.valid_token?(value)
      end
      {
        "trace_id" => format("ci-%d-%d", (Time.now.to_f * 1000).to_i, seq),
        "actor" => nil,
        "action_index" => 0,
        "build" => commit,
        "config_contract" => nil,
        "capture_envelope" => true,
      }
    end

    # Same envelope shape production capture records; the seed pins the
    # REPLAY run's randomness, it does not reproduce the test run's.
    def envelope_for(trace)
      first = trace.events[0] || {}
      ReproitBackendRb.determinism_envelope(first["at"].is_a?(Integer) ? first["at"] : nil)
    end

    def spool_dir
      dir = ENV["REPROIT_CI_SPOOL"].to_s
      dir.empty? ? DEFAULT_SPOOL_DIR : dir
    end

    def spool_max_bytes
      parsed = Integer(ENV["REPROIT_CI_SPOOL_MAX"].to_s, 10, exception: false)
      return DEFAULT_SPOOL_MAX_BYTES if parsed.nil?
      parsed.clamp(SPOOL_MAX_FLOOR_BYTES, SPOOL_MAX_CEIL_BYTES)
    end

    def record_drop(dir)
      counter = File.join(dir, "dropped.count")
      dropped = begin
        Integer(File.read(counter).strip, 10, exception: false) || 0
      rescue SystemCallError
        # First drop: the counter does not exist yet.
        0
      end
      File.write(counter, (dropped + 1).to_s + "\n")
    end

    # Write one capsule inside the byte cap; over-cap capsules are dropped
    # and counted. Returns the file path or nil.
    def spool(payload)
      body = ReproitBackendRb.canonical_json(payload)
      dir = spool_dir
      FileUtils.mkdir_p(dir)
      used = Dir.children(dir).sum do |entry|
        next 0 unless entry.end_with?(".json")
        begin
          File.size(File.join(dir, entry))
        rescue SystemCallError
          # A concurrently removed entry counts as zero.
          0
        end
      end
      if used + body.bytesize > spool_max_bytes
        count(:dropped_capsules)
        record_drop(dir)
        return nil
      end
      digest = Digest::SHA256.hexdigest(body)[0, 12]
      file = File.join(dir, "capsule-" + digest + ".json")
      File.write(file, body)
      count(:spooled_capsules)
      $stderr.write(
        SPOOL_MARKER + JSON.generate({ "file" => file, "operation" => payload["operation"] }) +
        "\n"
      )
      file
    end

    def finish_and_spool(trace, operation, error)
      trace.finish({ "error" => bounded_error(error) }, nil, false, false)
      spool({
        "format" => CAPTURE_FORMAT,
        "version" => CAPTURE_VERSION_EXCHANGES,
        "operation" => operation,
        "oracle" => TEST_FAILURE_ORACLE,
        "envelope" => envelope_for(trace),
        "events" => trace.events,
      })
    rescue StandardError
      # Capture must never mask the test's own failure.
      count(:failed_captures)
    end

    # The capsule names exactly one test; everything else is skipped so the
    # process exit code speaks for the named test alone.
    def replay_target
      @lock.synchronize do
        unless @replay_target_loaded
          @replay_target_loaded = true
          payload = JSON.parse(File.read(replay_path))
          operation = payload["operation"]
          unless operation.is_a?(String) && operation.start_with?(TEST_TRIGGER_PREFIX)
            raise ArgumentError,
                  "REPROIT_REPLAY capsule does not carry a test trigger identity"
          end
          @replay_target = operation
        end
        @replay_target
      end
    end

    def report_result(operation, status, error)
      detail = { "operation" => operation, "status" => status }
      detail["failure"] = bounded_error(error) unless error.nil?
      $stderr.write(RESULT_MARKER + JSON.generate(detail) + "\n")
    end

    # Every test runs inside its own trace with the Net::HTTP hook and db
    # helper live, so dependency exchanges and the envelope record exactly as
    # production capture does; a failing test spools the capsule and
    # re-raises, never masking the failure.
    def capture_body(operation, suite_name, test_name, fn)
      proc do
        trace = BackendTrace.begin(
          CI.ci_context, operation,
          input: { "suite" => CI.bounded_name(suite_name), "test" => CI.bounded_name(test_name) }
        )
        begin
          Instrument.with_trace(trace) { instance_exec(&fn) }
        rescue Minitest::Skip
          # A skipped test asserted nothing; there is no invariant to spool.
          raise
        rescue Exception => error
          # Minitest::Assertion descends from Exception, not StandardError,
          # so a bare rescue would miss the very failures this mode exists
          # to capture. The error is always re-raised.
          CI.finish_and_spool(trace, operation, error)
          raise
        end
        begin
          trace.finish(nil, nil, true, false)
        rescue TraceError
          # An over-long passing trace has nothing to spool anyway.
        end
      end
    end

    def replay_body(operation, target, fn)
      return proc { skip("reproit replay targets " + target) } if operation != target
      proc do
        begin
          instance_exec(&fn)
        rescue Minitest::Skip
          raise
        rescue Exception => error
          CI.report_result(operation, "failed", error)
          raise
        end
        CI.report_result(operation, "passed", nil)
      end
    end

    # One Minitest::Test subclass per suite, running tests in DECLARATION
    # order (never shuffled): the flaky wedge exists to capture
    # order-dependent state, so the capture run and the replay run must walk
    # the tests the same way, like the Node reference's node:test does.
    def suite_class(suite_name)
      seq = @lock.synchronize { @suite_seq += 1 }
      klass = Class.new(Minitest::Test) do
        @reproit_methods = []

        def self.runnable_methods
          @reproit_methods.dup
        end

        def self.reproit_register(method_name, body)
          define_method(method_name, &body)
          @reproit_methods << method_name
        end
      end
      # A named constant so Minitest reports read `ReproitCiSuiteN#test_...`
      # instead of an anonymous class.
      const_set("ReproitCiSuite#{seq}", klass)
      klass
    end

    # `options` is reserved; there are none yet and unknown keys are
    # rejected so a typo cannot silently change capture behavior.
    def suite(suite_name, options = {})
      unless options.empty?
        raise ArgumentError, "reproit ci.suite: unknown option #{options.keys.first}"
      end
      active = mode
      Instrument.install unless active == "off"
      target = active == "replay" ? replay_target : nil
      klass = suite_class(suite_name)
      lambda do |test_name, &fn|
        operation = CI.operation_for(suite_name, test_name)
        method_name = format(
          "test_%03d_%s",
          klass.runnable_methods.length,
          test_name.to_s.gsub(/[^A-Za-z0-9]+/, "_")
        )
        body =
          case active
          when "capture" then CI.capture_body(operation, suite_name, test_name, fn)
          when "replay" then CI.replay_body(operation, target, fn)
          else fn
          end
        klass.reproit_register(method_name, body)
        method_name
      end
    end
  end
end
