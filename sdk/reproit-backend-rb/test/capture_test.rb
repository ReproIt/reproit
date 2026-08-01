# Capture-mode parity tests against sdk/reproit-backend-rs/src/capture.rs.
# Run: ruby test/capture_test.rb
#
# Cross-language batch validation against the protocol mirror lives in
# sdk/test/backend_batch_test.js; here we pin the shapes and bounds directly
# and additionally round-trip one built batch through the mirror validator.

require "json"
require "minitest/autorun"
require "open3"

require_relative "test_helper"
require_relative "../lib/reproit_backend_rb"

class CaptureTest < Minitest::Test
  include AmbientCodeIdentity

  R = ReproitBackendRb

  def capture(overrides = {})
    config = { endpoint: "http://c/v1/events", api_key: "sk", app_id: "app-demo" }
    R::Capture.create(**config.merge(overrides))
  end

  def finished_trace(status, success)
    handle = capture(build: "1.2.3")
    trace = R::BackendTrace.begin(
      handle.context, "createOrder",
      input: { "body" => { "item" => "widget", "qty" => 2 } }
    )
    trace.effect("read", resource: "inventory", key: "widget")
    trace.finish({ "error" => "boom" }, status, success, true)
    trace
  end

  def batch_for(status, success)
    handle = capture(build: "1.2.3")
    trace = finished_trace(status, success)
    operation = { "operation" => "createOrder", "status" => status, "events" => trace.events.dup }
    handle.build_batch([operation])
  end

  # The env fallback used to be exercised only by accident, when GITHUB_SHA
  # leaked into the suite and broke the exact-shape assertions below. Pin it on
  # purpose instead: a deployment carries the identity its runner knows.
  def test_a_ci_runner_supplies_the_commit_the_config_omits
    sha = "f857cb7740a5f857cb7740a5f857cb7740a5f857"
    ENV["GITHUB_SHA"] = sha
    assert_equal({ "version" => "1.2.3", "commit" => sha }, batch_for(500, false)["deployment"])
  end

  def test_server_error_batch_uses_the_universal_causal_contract
    batch = batch_for(500, false)
    assert_equal 1, batch["version"]
    assert_equal "app-demo", batch["projectId"]
    assert_equal({ "version" => "1.2.3" }, batch["deployment"])
    events = batch["events"]
    assert_equal [1, 2, 3, 4, 5, 6, 7], events.map { |event| event["sequence"] }
    finding = events[6]["event"]
    assert_equal "observation", finding["kind"]
    assert_equal(
      R::SERVER_ERROR_ORACLE + ":createOrder",
      finding["failure"]["signature"]
    )
    # Redaction happened before anything left the process boundary.
    assert_equal "widget", events[1]["event"]["value"]["value"]["body"]["item"]
    # The determinism envelope rides as a named checkpoint after the trigger.
    envelope = events[2]["event"]
    assert_equal "checkpoint", envelope["kind"]
    assert_equal "determinism-envelope", envelope["name"]
    assert_kind_of Integer, envelope["attributes"]["observedAtMs"]
    assert_kind_of String, envelope["attributes"]["replaySeed"]
    # The raw return event is nested like the raw effects, under a subject
    # that names the carrier for the protocol projection.
    carrier = events[4]["event"]
    assert_equal "effect", carrier["kind"]
    assert_equal "operation-return", carrier["subject"]
    raw_return = carrier["value"]["value"]
    assert_equal "return", raw_return["kind"]
    assert_equal 500, raw_return["status"]
  end

  def test_healthy_operations_ship_causal_events_without_an_observation
    batch = batch_for(201, true)
    events = batch["events"]
    assert_equal 6, events.length
    assert(events.none? { |event| event["event"]["kind"] == "observation" })
  end

  def test_oversized_captures_drop_trailing_effects_first
    events = finished_trace(500, false).events.dup
    filler = "x" * R::MAX_CAPTURE_JSON_BYTES
    events.insert(2, { "kind" => "effect", "effect" => "write", "resource" => filler })
    payload, dropped = R.capture_payload(
      { "operation" => "createOrder", "status" => 500, "events" => events }
    )
    assert_equal 1, dropped
    kept = payload["events"]
    assert_equal 3, kept.length
    assert_equal "effect", kept[1]["kind"]
    assert_equal "inventory", kept[1]["resource"]
  end

  def test_payload_version_reflects_exchanges_and_envelope_stamps
    plain = [{ "kind" => "start", "operation" => "op" },
             { "kind" => "return", "status" => 500, "success" => false }]
    payload, = R.capture_payload({ "operation" => "op", "status" => 500, "events" => plain })
    assert_equal R::CAPTURE_VERSION, payload["version"]
    stamped = [{ "kind" => "start", "operation" => "op", "at" => 1, "monoNs" => 0 },
               { "kind" => "return", "status" => 500, "success" => false }]
    envelope = R.determinism_envelope(1_753_747_200_000)
    payload, = R.capture_payload(
      { "operation" => "op", "status" => 500, "events" => stamped }, envelope
    )
    assert_equal R::CAPTURE_VERSION_EXCHANGES, payload["version"]
    assert_equal envelope, payload["envelope"]
    assert_match(/\A[0-9a-f]{16}\z/, payload["envelope"]["replaySeed"])
  end

  def test_capture_that_cannot_fit_start_plus_return_is_omitted
    events = [
      {
        "kind" => "start", "operation" => "op",
        "input" => { "blob" => "x" * R::MAX_CAPTURE_JSON_BYTES }
      },
      { "kind" => "return", "status" => 500, "success" => false },
    ]
    payload, = R.capture_payload({ "operation" => "op", "status" => 500, "events" => events })
    assert_nil payload
  end

  def test_unusable_configs_disable_capture_instead_of_failing
    assert_nil R::Capture.create(endpoint: "", api_key: "sk", app_id: "app")
    assert_nil R::Capture.create(endpoint: "http://c", api_key: "", app_id: "app")
    assert_nil R::Capture.create(endpoint: "http://c", api_key: "sk", app_id: "bad app id")
    assert_nil R::Capture.create(
      endpoint: "http://c", api_key: "sk", app_id: "app", build: "bad build"
    )
  end

  def test_record_samples_failures_only_by_default
    handle = capture
    open_trace = R::BackendTrace.begin(handle.context, "op")
    handle.record(open_trace)
    healthy = R::BackendTrace.begin(handle.context, "op")
    healthy.finish(nil, 200, true, true)
    handle.record(healthy)
    assert_equal 0, handle.stats[:captured_operations]
    failed = R::BackendTrace.begin(handle.context, "op")
    failed.finish(nil, 200, false, true)
    handle.record(failed)
    assert_equal 1, handle.stats[:captured_operations]
    assert_equal true, handle.flush(10.0)
    stats = handle.stats
    # http://c is unreachable: the batch fails and its operation is dropped.
    assert_equal 1, stats[:failed_batches]
    assert_equal 1, stats[:dropped_operations]
  end

  def test_built_batch_has_dense_causal_parentage
    batch_for(500, false)["events"].each_with_index do |event, index|
      assert_equal index + 1, event["sequence"]
      expected = index.zero? ? [] : ["evt_backend-ruby_#{index}"]
      assert_equal expected, event["causalParentIds"]
    end
  end
end
