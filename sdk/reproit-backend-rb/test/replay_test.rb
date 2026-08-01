# Hermetic replay tests: with REPROIT_REPLAY set, the Net::HTTP hook and the
# database helper serve recorded exchanges with no socket and no driver,
# divergence fails closed with the structured marker, and the envelope pins
# TZ and the seeded stream.
#
# Run: ruby test/replay_test.rb

require "json"
require "minitest/autorun"
require "net/http"
require "tmpdir"

CAPTURE = {
  "format" => "reproit-backend-capture",
  "version" => 2,
  "operation" => "GET /quote",
  "oracle" => "backend-server-error",
  "envelope" => {
    "observedAtMs" => 1_753_747_200_000,
    "tz" => "UTC",
    "runtime" => "ruby",
    "replaySeed" => "00ff00ff00ff00ff",
  },
  "events" => [
    { "kind" => "start", "operation" => "GET /quote", "sequence" => 1 },
    {
      "kind" => "effect", "effect" => "read", "sequence" => 2,
      "exchange" => {
        "protocol" => "db",
        "request" => { "text" => "SELECT id FROM issuers WHERE symbol = $1",
                       "values" => ["ACME"] },
        "response" => { "command" => "SELECT", "rowCount" => 1, "rows" => [{ "id" => 7 }] },
      },
    },
    {
      "kind" => "effect", "effect" => "call", "sequence" => 3,
      "exchange" => {
        "protocol" => "http",
        "request" => { "method" => "GET", "url" => "http://pricing.internal/prices?tier=gold" },
        "response" => {
          "status" => 200,
          "headers" => { "content-type" => "application/json" },
          "body" => { "prices" => nil },
        },
      },
    },
    {
      "kind" => "effect", "effect" => "read", "sequence" => 4,
      "exchange" => {
        "protocol" => "pg",
        "request" => { "text" => "SELECT symbol FROM issuers WHERE id = $1", "values" => [7] },
        "response" => { "command" => "SELECT", "rowCount" => 1,
                        "rows" => [{ "symbol" => "ACME" }] },
      },
    },
    { "kind" => "return", "status" => 500, "success" => false, "sequence" => 5 },
  ],
}.freeze

CAPTURE_PATH = File.join(Dir.tmpdir, "reproit-rb-replay-#{Process.pid}.json")
File.write(CAPTURE_PATH, JSON.generate(CAPTURE))
ENV["REPROIT_REPLAY"] = CAPTURE_PATH

require_relative "../lib/reproit_backend_rb"

class ReplayTest < Minitest::Test
  R = ReproitBackendRb

  def setup
    R::Instrument.install
  end

  def test_envelope_pins_tz_and_seeds_the_stream
    assert_equal "UTC", ENV["TZ"]
    rng = R::Instrument.replay_rng
    refute_nil rng
    draw = rng.next_float
    assert draw >= 0 && draw < 1
  end

  def test_the_wall_clock_is_anchored_to_the_capture_instant
    # Replayed code reading Time.now sees the capture's moment, not today's,
    # so time-dependent behavior reproduces instead of drifting by however
    # long ago the capture was taken.
    drift_ms = ((Time.now.to_f * 1000) - 1_753_747_200_000).abs
    assert drift_ms < 60_000, "wall clock not anchored: drift #{drift_ms}ms"
    realtime_ms = Process.clock_gettime(Process::CLOCK_REALTIME, :millisecond)
    assert ((realtime_ms - 1_753_747_200_000).abs < 60_000),
           "CLOCK_REALTIME not anchored"
  end

  def test_the_anchored_clock_still_elapses
    # Offsetting, not freezing: a timeout loop must still terminate.
    first = Time.now
    sleep 0.01
    assert Time.now > first, "an anchored clock must still advance"
  end

  def test_the_monotonic_clock_is_left_alone
    # Anchoring the monotonic clock would corrupt duration math, which reads
    # differences rather than instants.
    assert Process.clock_gettime(Process::CLOCK_MONOTONIC) < 1e11
  end

  def test_database_calls_serve_recorded_rows_without_a_driver
    result = R::Instrument.db("SELECT id FROM issuers WHERE symbol = $1", ["ACME"]) do
      raise "the live driver must never be reached during hermetic replay"
    end
    assert_equal [{ "id" => 7 }], result["rows"]
  end

  def test_http_calls_serve_the_recorded_response_without_a_socket
    response = Net::HTTP.get_response(URI("http://pricing.internal/prices?tier=gold"))
    assert_equal "200", response.code
    assert_equal({ "prices" => nil }, JSON.parse(response.body))
  end

  def test_an_unmatched_call_diverges_with_the_structured_marker
    captured = +""
    original = $stderr
    $stderr = StringIO.new(captured)
    begin
      response = Net::HTTP.get_response(URI("http://pricing.internal/unknown"))
      assert_equal "599", response.code
      assert_equal({ "reproit" => "diverged" }, JSON.parse(response.body))
    ensure
      captured = $stderr.string
      $stderr = original
    end
    # The line must START with the marker: the CLI matches on the prefix, so
    # anything Ruby prepends (Kernel#warn's "file:line: warning: ") would
    # make the divergence invisible to the verdict machinery.
    marker = captured.lines.find { |line| line.start_with?(R::Replay::DIVERGENCE_MARKER) }
    refute_nil marker, "structured divergence marker emitted at line start"
    report = JSON.parse(marker[R::Replay::DIVERGENCE_MARKER.length..])
    assert_equal "http", report["protocol"]
    assert_equal "GET", report["got"]["method"]
  end

  def test_kernel_rand_is_seeded_from_the_envelope
    # Two pins of the same seed must give the same Kernel#rand stream:
    # replayed application draws are repeatable across runs. SecureRandom
    # reads OS entropy and stays unpinnable, a named gap.
    R::Replay.pin_rand("00ff00ff00ff00ff")
    first = Array.new(4) { rand }
    R::Replay.pin_rand("00ff00ff00ff00ff")
    assert_equal first, Array.new(4) { rand }
  end

  def test_wrapped_pg_serves_recorded_rows_without_dialing
    fake = Module.new do
      const_set(:Connection, Class.new do
        def self.connect(*)
          raise "live database dialed during hermetic replay"
        end

        def exec_params(_text, _values = nil)
          raise "live database reached during hermetic replay"
        end
      end)
    end
    R.wrap_pg(fake)
    connection = fake.const_get(:Connection).connect("postgresql://db.internal/quotes")
    result = connection.exec_params("SELECT symbol FROM issuers WHERE id = $1", [7])
    assert_equal [{ "symbol" => "ACME" }], result.to_a
    assert_equal "SELECT", result.cmd_status
    assert_raises(R::Instrument::DivergenceError) do
      connection.exec_params("SELECT * FROM never_recorded", [])
    end
  end

  def session_for(events)
    R::Replay::Session.new(
      "format" => "reproit-backend-capture", "version" => 2,
      "operation" => "GET /x", "oracle" => "backend-server-error",
      "events" => events.each_with_index.map do |exchange, index|
        { "kind" => "effect", "sequence" => index + 1, "exchange" => exchange }
      end
    )
  end

  def http_exchange(url, body, method: "GET")
    {
      "protocol" => "http",
      "request" => { "method" => method, "url" => url },
      "response" => { "status" => 200, "headers" => {}, "body" => body },
    }
  end

  def test_matching_is_per_operation_ordinal_so_operations_interleave
    session = session_for([
      http_exchange("http://svc/a", "a1"),
      http_exchange("http://svc/b", "b1"),
      http_exchange("http://svc/a", "a2"),
    ])
    # Consuming /b before either /a is fine (interleaved operations), but
    # within /a the recorded order is strict.
    served = %w[b a a].map do |op|
      session.match("http", { "method" => "GET", "url" => "http://svc/#{op}" })
    end
    assert_equal %w[b1 a1 a2], (served.map { |hit| hit["response"]["body"] })
  end

  def divergence_report(session, probe)
    captured = StringIO.new
    original = $stderr
    begin
      $stderr = captured
      assert_nil session.match("http", probe)
    ensure
      $stderr = original
    end
    line = captured.string.lines.find { |l| l.start_with?(R::Replay::DIVERGENCE_MARKER) }
    refute_nil line
    JSON.parse(line[R::Replay::DIVERGENCE_MARKER.length..])
  end

  def test_prompt_drift_names_the_first_differing_message_index
    recorded = { "messages" => [
      { "role" => "user", "content" => "hello" },
      { "role" => "assistant", "content" => "hi" },
    ] }
    session = session_for([http_exchange("http://llm/v1/chat", nil, method: "POST")
      .tap { |x| x["request"]["body"] = recorded }])
    live = { "messages" => [
      { "role" => "user", "content" => "hello" },
      { "role" => "assistant", "content" => "DIFFERENT" },
    ] }
    report = divergence_report(
      session, { "method" => "POST", "url" => "http://llm/v1/chat", "body" => live }
    )
    assert_equal(
      { "kind" => "message", "firstDifferingMessage" => 1,
        "recordedMessages" => 2, "liveMessages" => 2 },
      report["bodyDelta"]
    )
  end

  def test_non_chat_body_drift_falls_back_to_the_byte_offset
    session = session_for([http_exchange("http://svc/v", nil, method: "POST")
      .tap { |x| x["request"]["body"] = "abcdef" }])
    report = divergence_report(
      session, { "method" => "POST", "url" => "http://svc/v", "body" => "abcXef" }
    )
    assert_equal({ "kind" => "byte", "offset" => 3 }, report["bodyDelta"])
  end

  def test_a_missing_live_body_reports_no_delta_but_still_diverges
    # ABSENT is not null: a recorded null body matches anything, but a
    # recorded REAL body probed without one diverges with no bodyDelta,
    # because there is no live body to locate the difference in.
    session = session_for([http_exchange("http://svc/v", nil, method: "POST")
      .tap { |x| x["request"]["body"] = { "a" => 1 } }])
    report = divergence_report(session, { "method" => "POST", "url" => "http://svc/v" })
    refute report.key?("bodyDelta")
  end

  def test_recorded_stream_shape_is_served_chunk_for_chunk
    exchange = http_exchange("http://llm/stream", "data: a\n\ndata: b\n\n")
    exchange["response"]["stream"] = { "chunks" => [9, 9] }
    session = session_for([exchange])
    served = R::Replay.serve_http(session, { "method" => "GET", "url" => "http://llm/stream" })
    assert_equal ["data: a\n\n", "data: b\n\n"], served["chunks"]
  end

  def test_truncated_stream_boundaries_fail_closed
    exchange = http_exchange("http://llm/stream2", "data: a\n\n")
    exchange["response"]["stream"] = { "chunks" => [9], "truncated" => true }
    session = session_for([exchange])
    captured = StringIO.new
    original = $stderr
    begin
      $stderr = captured
      served = R::Replay.serve_http(session, { "method" => "GET", "url" => "http://llm/stream2" })
    ensure
      $stderr = original
    end
    assert_equal 599, served["status"]
    assert_equal({ "reproit" => "truncated-stream-boundaries" }, JSON.parse(served["body"]))
  end

  Minitest.after_run { File.delete(CAPTURE_PATH) if File.exist?(CAPTURE_PATH) }
end
