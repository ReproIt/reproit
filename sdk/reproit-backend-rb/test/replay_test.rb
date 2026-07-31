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
    { "kind" => "return", "status" => 500, "success" => false, "sequence" => 4 },
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

  Minitest.after_run { File.delete(CAPTURE_PATH) if File.exist?(CAPTURE_PATH) }
end
