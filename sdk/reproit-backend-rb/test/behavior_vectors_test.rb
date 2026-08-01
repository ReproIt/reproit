# Shared behavioral conformance vectors (sdk/capture-behavior-v1.json).
#
# Eleven SDKs hand implement one contract, so a defect otherwise has to be
# found eleven times. Ruby's own instance was the divergence marker written
# through `warn`, whose "file:line: warning:" prefix broke the CLI's match, so
# a diverged run reported as reproduced. The divergence group pins that the
# marker STARTS the line.
#
# What the other groups pin, and the real defect behind each:
#
#   bounds                   the inline body budget is BYTES, not characters.
#                            4096 euro signs are 12288 bytes; a runtime
#                            measuring string length records that inline and
#                            the capsule blows a budget replay trusts.
#   headers                  names lowercase, and the 32 header cap is taken
#                            over NAME SORTED order. Go capped a randomized
#                            map in arrival order and recorded a different
#                            subset every run, so replay was unrepeatable.
#   redaction typeCases      the placeholder carries type and length.
#   redaction foldingCases   which field names fold to secret.
#   redaction nestingCases   redaction reaches nested objects and arrays.
#   redaction structureCases redaction is structure preserving: no key
#                            dropped, no array shortened, an explicit null
#                            still a null value. An encoder that dropped null
#                            map values changed the shape the replay matcher
#                            walks, and replay reproduced a DIFFERENT error.

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
    assert_equal VECTORS["constants"]["maxExchangeBodyBytes"],
                 ReproitBackendRb::Exchange::MAX_EXCHANGE_BODY_BYTES
    assert_equal VECTORS["constants"]["maxExchangeHeaders"],
                 ReproitBackendRb::Exchange::MAX_EXCHANGE_HEADERS
    assert_equal VECTORS["constants"]["maxPgRows"],
                 ReproitBackendRb::Exchange::MAX_DB_ROWS
  end

  # The replay matcher is the other half of capture parity: the recorded
  # side of each case is what THIS SDK writes, the live side is what a
  # replayed app sends, and the verdicts must agree across every SDK.
  def test_matching_vectors
    VECTORS["matching"]["cases"].each do |kase|
      actual = ReproitBackendRb::Replay.request_matches?("http", kase["recorded"], kase["live"])
      assert_equal kase["expect"]["matches"], actual, "matching case #{kase['name']}"
    end
  end

  def test_pg_matching_vectors
    VECTORS["matching"]["pgCases"].each do |kase|
      actual = ReproitBackendRb::Replay.request_matches?("pg", kase["recorded"], kase["live"])
      assert_equal kase["expect"]["matches"], actual, "pg matching case #{kase['name']}"
    end
  end

  # `bodyRepeat` keeps the vectors small on disk; expand both sides.
  def self.expand(spec)
    return spec["bodyRepeat"][0] * spec["bodyRepeat"][1] if spec["bodyRepeat"]
    spec["body"]
  end

  def test_bounds_vectors
    VECTORS["bounds"]["cases"].each do |kase|
      body = self.class.expand(kase["input"])
      expect = kase["expect"].dup
      if expect["body"].is_a?(Hash) && expect["body"]["repeat"]
        expect["body"] = expect["body"]["repeat"][0] * expect["body"]["repeat"][1]
      end
      actual = ReproitBackendRb::Exchange.bounded_body(body, kase["input"]["contentType"])
      assert_equal expect, actual, "bounds case #{kase['name']}"
    end
  end

  # The cap case is fed in a deterministic NON-sorted order, so a cap taken
  # over arrival order keeps the wrong subset and says so.
  def test_headers_vectors
    VECTORS["headers"]["cases"].each do |kase|
      if kase["input"]
        actual = ReproitBackendRb::Exchange.bounded_headers(kase["input"]["headers"])
        assert_equal kase["expect"], actual, "headers case #{kase['name']}"
        next
      end
      spec = kase["inputGenerated"]
      count = spec["headerCount"]
      shuffled = {}
      # 17 is coprime with 40, so this walks every name exactly once.
      count.times do |index|
        shuffled[format(spec["namePattern"], (index * 17) % count)] = spec["value"]
      end
      names = ReproitBackendRb::Exchange.bounded_headers(shuffled)["headers"].keys
      assert_equal kase["expect"]["headerCount"], names.length, "headers case #{kase['name']}"
      assert_equal kase["expect"]["firstName"], names.first,
                   "the cap must be taken over sorted names, not arrival order"
      assert_equal kase["expect"]["lastName"], names.last,
                   "the cap must be taken over sorted names, not arrival order"
    end
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

  def test_redaction_nesting_vectors
    VECTORS["redaction"]["nestingCases"].each do |kase|
      assert_equal kase["expect"], ReproitBackendRb.redact(kase["input"]), kase["input"].inspect
    end
  end

  # Structure preservation: a dropped key, a shortened array or a collapsed
  # null all change the shape the replay matcher walks.
  def test_redaction_structure_vectors
    VECTORS["redaction"]["structureCases"].each do |kase|
      actual = ReproitBackendRb.redact(kase["input"])
      assert_equal kase["expect"], actual, "structure case #{kase['name']}"
    end
  end

  # The Ruby defect: `warn` prepends "file:line: warning:" so the marker no
  # longer STARTS the line and the CLI's prefix match failed, turning a
  # divergence into a reported reproduction.
  def test_divergence_marker_starts_the_line
    kase = VECTORS["divergence"]["cases"][0]
    capsule = {
      "format" => "reproit-backend-capture",
      "version" => 2,
      "operation" => "GET /x",
      "oracle" => "backend-server-error",
      "events" => kase["capsuleExchanges"].each_with_index.map do |exchange, index|
        { "kind" => "effect", "sequence" => index + 1, "exchange" => exchange }
      end,
    }
    session = ReproitBackendRb::Replay::Session.new(capsule)

    captured = StringIO.new
    original = $stderr
    begin
      $stderr = captured
      hit = session.match("http", { "method" => kase["live"]["method"],
                                    "url" => kase["live"]["url"] })
      assert_nil hit, "an unmatched call must not serve an exchange"
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
    assert_equal kase["expect"]["consumed"], report["consumed"]
    assert_equal kase["expect"]["total"], report["total"]
    assert_equal kase["expect"]["expectedRequest"], report["expected"]
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
