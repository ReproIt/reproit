# Shared behavioral conformance vectors (sdk/capture-behavior-v1.json).
#
# Eleven SDKs hand implement one contract, so a defect otherwise has to be
# found eleven times. Ruby's own instance was the divergence marker written
# through `warn`, whose "file:line: warning:" prefix broke the CLI's match, so
# a diverged run reported as reproduced. The divergence group pins that the
# marker STARTS the line.

require "json"
require "minitest/autorun"
require "stringio"

require_relative "../lib/reproit_backend_rb"

class BehaviorVectorsTest < Minitest::Test
  VECTORS = JSON.parse(
    File.read(File.expand_path("../../capture-behavior-v1.json", __dir__)),
  )

  def test_constants_match_the_shared_vectors
    assert_equal VECTORS["constants"]["divergenceMarker"],
                 ReproitBackendRb::Replay::DIVERGENCE_MARKER
  end

  def test_redaction_type_vectors
    VECTORS["redaction"]["typeCases"].each do |kase|
      actual = ReproitBackendRb.redact(kase["input"])
      assert_equal kase["expect"], actual, kase["input"].inspect
    end
  end

  def test_redaction_key_folding_vectors
    VECTORS["redaction"]["foldingCases"].each do |kase|
      out = ReproitBackendRb.redact({ kase["field"] => "value" })
      value = out[kase["field"]]
      redacted = value.is_a?(Hash) && value.key?("$reproit")
      assert_equal kase["secret"], redacted,
                   "#{kase['field']} should #{kase['secret'] ? '' : 'not '}be secret"
    end
  end

  # The Ruby defect: `warn` prepends "file:line: warning:" so the marker no
  # longer STARTS the line and the CLI's prefix match failed, turning a
  # divergence into a reported reproduction.
  def test_divergence_marker_starts_the_line
    capsule = {
      "format" => "reproit-backend-capture",
      "version" => 2,
      "operation" => "GET /x",
      "oracle" => "backend-server-error",
      "events" => [
        {
          "kind" => "effect",
          "sequence" => 1,
          "exchange" => {
            "protocol" => "http",
            "request" => { "method" => "GET", "url" => "http://svc/prices" },
          },
        },
      ],
    }
    session = ReproitBackendRb::Replay::Session.new(capsule)

    captured = StringIO.new
    original = $stderr
    begin
      $stderr = captured
      session.match("http", { "method" => "GET", "url" => "http://svc/unknown" })
    ensure
      $stderr = original
    end

    marker = VECTORS["divergence"]["markerPrefix"]
    lines = captured.string.lines
    hit = lines.find { |line| line.start_with?(marker) }
    refute_nil hit,
               "the marker must START the line; got #{captured.string.inspect}"

    report = JSON.parse(hit[marker.length..].strip)
    VECTORS["divergence"]["reportFields"]["required"].each do |field|
      assert report.key?(field), "report is missing required field #{field}"
    end
  end

  def test_trigger_token_is_in_the_protocol_vocabulary
    token = VECTORS["triggerTokens"]["bySdkKind"]["backend"]
    assert_includes VECTORS["triggerTokens"]["allowed"], token
    source = File.read(File.expand_path("../lib/reproit_backend_rb/capture.rb", __dir__))
    assert_includes source, token
    VECTORS["triggerTokens"]["rejected"].each do |bad|
      refute_includes source, "\"#{bad}\""
      refute_includes source, "'#{bad}'"
    end
  end
end
