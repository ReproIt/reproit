# Semantics parity tests against sdk/reproit-backend-rs/src/lib.rs.
# Run: ruby test/trace_test.rb

require "json"
require "minitest/autorun"

require_relative "../lib/reproit_backend_rb"

class TraceTest < Minitest::Test
  R = ReproitBackendRb

  def context(overrides = {})
    {
      "trace_id" => "trace-a",
      "actor" => nil,
      "action_index" => 0,
      "build" => nil,
      "config_contract" => nil,
    }.merge(overrides)
  end

  def decode_header(header)
    padded = header + "=" * ((4 - header.length % 4) % 4)
    JSON.parse(padded.tr("-_", "+/").unpack1("m0"))
  end

  def test_emits_bounded_correlated_redacted_events
    headers = {
      "x-reproit-trace" => "trace-a",
      "x-reproit-actor" => "alice",
      "x-reproit-action" => "7",
      "x-reproit-build" => "build-a",
      "x-reproit-config-contract" => "contract-a",
    }
    trace_context = R.trace_context_from_headers(->(name) { headers[name] })
    trace = R::BackendTrace.begin(
      trace_context,
      "createProject",
      tenant: "org-1",
      idempotency_key: "retry-secret",
      input: { "name" => "demo", "password" => "abcdefgh" },
      selections: [R.selection("project.id", "projectId")]
    )
    trace.effect("write", resource: "projects", key: "1", tenant: "org-1")
    trace.finish(
      {
        "id" => 1,
        "apiKey" => "sk_live_secret",
        "publishable_key" => "pk_live_secret",
        "private-key" => "private-secret",
        "access key" => "access-secret",
        "signingKey" => "signing-secret",
        "monkey" => "harmless",
      },
      201, true, true
    )
    assert_operator trace.header.length, :<, R::MAX_HEADER_BYTES
    events = trace.events
    assert_equal 7, events[0]["actionIndex"]
    assert_equal "build-a", events[0]["build"]
    assert_equal "contract-a", events[0]["configContract"]
    assert_equal 8, events[0]["input"]["password"]["$reproit"]["length"]
    refute_equal "retry-secret", events[0]["idempotencyKey"]
    assert events[0]["idempotencyKey"].start_with?("sha256:")
    %w[apiKey publishable_key private-key access\ key signingKey].each do |field|
      assert_equal true, events[2]["output"][field]["$reproit"]["redacted"]
    end
    assert_equal "harmless", events[2]["output"]["monkey"]
    assert_equal true, events[2]["effectsComplete"]
  end

  def test_stays_inactive_without_a_trace_header
    assert_nil R.trace_context_from_headers(->(_name) { nil })
    headers = { "x-reproit-trace" => "  " }
    assert_nil R.trace_context_from_headers(->(name) { headers[name] })
  end

  def test_header_is_unpadded_base64url_of_canonical_json
    trace = R::BackendTrace.begin(context, "op", input: { "b" => 1, "a" => 2 })
    trace.finish({ "ok" => true }, 200, true, true)
    header = trace.header
    refute_match(/[=+\/]/, header)
    decoded = decode_header(header)
    assert_equal JSON.parse(JSON.generate(trace.events)), decoded
    raw = (header + "=" * ((4 - header.length % 4) % 4)).tr("-_", "+/").unpack1("m0")
    assert_operator raw.index('"a":2'), :<, raw.index('"b":1')
  end

  def test_one_return_and_no_effects_after_return
    trace = R::BackendTrace.begin(context, "op")
    trace.finish(nil, 200, true, false)
    error = assert_raises(R::TraceError) { trace.effect("read") }
    assert_equal "AlreadyFinished", error.code
    assert_raises(R::TraceError) { trace.finish(nil, 200, true, false) }
  end

  def test_header_bounds
    trace = R::BackendTrace.begin(context, "op")
    unfinished = assert_raises(R::TraceError) { trace.header }
    assert_equal "AlreadyFinished", unfinished.code
    big = R::BackendTrace.begin(context, "op")
    big.finish({ "blob" => "x" * R::MAX_HEADER_BYTES }, 200, true, true)
    oversized = assert_raises(R::TraceError) { big.header }
    assert_equal "HeaderTooLarge", oversized.code
  end

  def test_event_count_is_capped
    trace = R::BackendTrace.begin(context, "op")
    (R::MAX_EVENTS - 1).times { trace.effect("emit", event: "tick") }
    error = assert_raises(R::TraceError) { trace.effect("emit") }
    assert_equal "TooManyEvents", error.code
  end

  def test_typed_effects_and_bounded_identifiers
    trace = R::BackendTrace.begin(context, "op")
    assert_raises(R::TraceError) { trace.effect("mutate") }
    assert_raises(R::TraceError) { R::BackendTrace.begin(context, "") }
    assert_raises(R::TraceError) { R::BackendTrace.begin(context, "x" * 257) }
  end

  def test_effect_detail_keeps_only_before_after_payload
    trace = R::BackendTrace.begin(context, "op")
    trace.effect(
      "write",
      resource: "users",
      detail: {
        "before" => { "email" => "a@b.c" },
        "after" => { "name" => "z" },
        "extra" => "dropped",
      }
    )
    effect = trace.events[1]
    assert_equal true, effect["before"]["email"]["$reproit"]["redacted"]
    assert_equal "z", effect["after"]["name"]
    refute effect.key?("extra")
  end

  def test_canonical_http_input
    value = R.http_input(
      body: { "name" => "demo" },
      path: { "project" => "p1" },
      query: { "tag" => %w[a b] },
      headers: { "X-Mode" => "safe" }
    )
    assert_equal "safe", value["headers"]["x-mode"]
    assert_equal %w[a b], value["query"]["tag"]
    assert_equal({}, R.http_input(path: {}, query: {}, headers: {}))
  end

  def test_selections_validate_their_paths
    refute_nil R.selection("project.id", "projectId")
    refute_nil R.selection("items[].id", "rows[].id", "Widget")
    assert_nil R.selection("1bad", "ok")
    assert_nil R.selection("ok", "ok", "Bad.Condition")
  end

  def test_canonical_json_matches_the_node_adapter_byte_for_byte
    # Wire-parity spot check against the canonical Node implementation: the
    # same logical document must encode to identical bytes.
    node_sdk = File.expand_path("../../reproit-backend-node/index.js", __dir__)
    skip "node reference SDK not present" unless File.exist?(node_sdk)
    value = {
      "z" => [1, true, nil, "text", { "b" => 2, "a" => { "y" => [], "x" => {} } }],
      "a" => "snowman \u2603 and \"quotes\" and \\slashes\n",
      "m" => 0,
    }
    script = "const {canonicalJson}=require(process.argv[1]);" \
      "let raw='';process.stdin.on('data',(c)=>raw+=c);" \
      "process.stdin.on('end',()=>process.stdout.write(canonicalJson(JSON.parse(raw))));"
    require "open3"
    out, status = Open3.capture2("node", "-e", script, node_sdk, stdin_data: JSON.generate(value))
    assert status.success?, "node canonicalJson invocation failed"
    assert_equal out, R.canonical_json(value)
  end
end
