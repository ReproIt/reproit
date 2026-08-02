# CI capture mode (ci.rb), mirroring the Node reference's test/ci.test.js: a
# failing test spools a test-trigger capsule, a replay run re-executes only
# the named test and reports the structured result marker, and the spool cap
# drops loudly. Each scenario runs the ci wrapper in a child process because
# capture/replay mode is decided by env at suite() time and
# Instrument.install rewires Net::HTTP process-wide.
# Run: ruby test/ci_test.rb

require "json"
require "minitest/autorun"
require "open3"
require "rbconfig"
require "tmpdir"

require_relative "test_helper"
require_relative "../lib/reproit_backend_rb"

class CiTest < Minitest::Test
  R = ReproitBackendRb

  SDK = File.expand_path("..", __dir__)

  # One upstream call, one assertion that fails unless FIXED=1. The upstream
  # stub only boots outside replay, exactly like a real suite's dependencies.
  FIXTURE = <<~'RUBY'
    require "json"
    require "net/http"
    require "socket"
    require ENV.fetch("REPROIT_SDK") + "/lib/reproit_backend_rb"

    PORT = 19_994
    if ENV["REPROIT_REPLAY"].to_s.empty?
      server = TCPServer.new("127.0.0.1", PORT)
      Thread.new do
        loop do
          client = server.accept
          begin
            client.gets
            loop { break if client.gets.to_s.strip.empty? }
            body = JSON.generate({ "n" => 7 })
            client.write(
              "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n" \
              "content-length: #{body.bytesize}\r\nconnection: close\r\n\r\n" + body
            )
          ensure
            client.close
          end
        end
      end
    end

    t = ReproitBackendRb::CI.suite("unit")
    t.call("asserts the upstream answer") do
      response = Net::HTTP.get_response(URI("http://127.0.0.1:#{PORT}/n"))
      body = JSON.parse(response.body)
      assert_equal(ENV["FIXED"] == "1" ? 7 : 8, body["n"])
    end
  RUBY

  def run_fixture(env)
    Open3.capture3(
      env.merge("REPROIT_SDK" => SDK), RbConfig.ruby, "-e", FIXTURE
    )
  end

  def with_spool(label)
    Dir.mktmpdir("reproit-ci-#{label}-") { |dir| yield dir }
  end

  def test_a_failing_test_spools_a_test_trigger_capsule_with_the_exchange
    with_spool("spool") do |spool|
      _out, err, status = run_fixture("REPROIT_CI_CAPTURE" => "1", "REPROIT_CI_SPOOL" => spool)
      refute status.success?
      assert_includes err, R::CI::SPOOL_MARKER
      files = Dir.children(spool).select { |name| name.start_with?("capsule-") }
      assert_equal 1, files.length
      capsule = JSON.parse(File.read(File.join(spool, files[0])))
      assert_equal "reproit-backend-capture", capsule["format"]
      assert_equal 2, capsule["version"]
      assert_equal "test:unit#asserts the upstream answer", capsule["operation"]
      assert_equal R::CI::TEST_FAILURE_ORACLE, capsule["oracle"]
      assert_kind_of String, capsule["envelope"]["replaySeed"]
      exchanges = capsule["events"].select { |event| event["exchange"] }
      assert_equal 1, exchanges.length
      assert_equal 7, exchanges[0]["exchange"]["response"]["body"]["n"]
      returned = capsule["events"].last
      assert_equal false, returned["success"]
      assert_includes returned["output"]["error"].to_s, "Expected: 8"
    end
  end

  def test_replay_reruns_the_named_test_and_reports_failed_then_passed
    with_spool("replay") do |spool|
      _out, _err, status = run_fixture(
        "REPROIT_CI_CAPTURE" => "1", "REPROIT_CI_SPOOL" => spool
      )
      refute status.success?
      file = Dir.children(spool)
        .select { |name| name.start_with?("capsule-") }
        .map { |name| File.join(spool, name) }
        .first
      refute_nil file
      # No upstream exists in either replay run; the SDK serves the recording.
      _out, err, failed = run_fixture("REPROIT_REPLAY" => file)
      refute failed.success?
      line = err.lines.find { |item| item.start_with?(R::CI::RESULT_MARKER) }
      refute_nil line, err
      report = JSON.parse(line.delete_prefix(R::CI::RESULT_MARKER))
      assert_equal "failed", report["status"]
      assert_equal "test:unit#asserts the upstream answer", report["operation"]
      assert_includes report["failure"].to_s, "Expected: 8"
      _out, err, passed = run_fixture("REPROIT_REPLAY" => file, "FIXED" => "1")
      assert passed.success?, err
      assert_includes err, '"status":"passed"'
    end
  end

  def test_a_full_spool_drops_the_capsule_and_counts_the_drop
    with_spool("full") do |spool|
      # Pre-fill the spool to the floor cap so the next capsule cannot fit.
      File.write(File.join(spool, "existing.json"), "x" * (4 * 1024))
      _out, _err, status = run_fixture(
        "REPROIT_CI_CAPTURE" => "1",
        "REPROIT_CI_SPOOL" => spool,
        "REPROIT_CI_SPOOL_MAX" => (4 * 1024).to_s
      )
      refute status.success?
      capsules = Dir.children(spool).select { |name| name.start_with?("capsule-") }
      assert_empty capsules
      assert_equal 1, Integer(File.read(File.join(spool, "dropped.count")).strip, 10)
    end
  end

  def test_without_capture_or_replay_env_the_wrapper_is_inert_minitest
    _out, err, status = run_fixture({})
    refute status.success?
    refute_includes err, R::CI::SPOOL_MARKER
    refute_includes err, R::CI::RESULT_MARKER
  end

  def test_unknown_suite_options_are_rejected_not_ignored
    error = assert_raises(ArgumentError) { R::CI.suite("s", { retries: 2 }) }
    assert_match(/unknown option/, error.message)
  end
end
