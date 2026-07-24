# Capture-mode parity tests against sdk/reproit-backend-rs/src/capture.rs.
# Run: ruby test/capture_test.rb
#
# Cross-language batch validation against the protocol mirror lives in
# sdk/test/backend_batch_test.js; here we pin the shapes and bounds directly
# and additionally round-trip one built batch through the mirror validator.

require "json"
require "minitest/autorun"
require "open3"

require_relative "../lib/reproit_backend_rb"

class CaptureTest < Minitest::Test
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

  def test_server_error_batch_is_a_tagged_event_batch
    batch = batch_for(500, false)
    assert_equal 1, batch["version"]
    assert_equal({ "version" => "1.2.3" }, batch["deployment"])
    frames = batch["frames"]
    assert_equal 4, frames.length
    assert_equal [1, 2, 3, 4], frames.map { |frame| frame["sequence"] }
    finding = frames[3]["event"]
    assert_equal "finding", finding["kind"]
    assert_equal R::SERVER_ERROR_ORACLE, finding["identity"]["oracle"]
    replay = finding["context"]["reproitCapture"]
    assert_equal R::CAPTURE_FORMAT, replay["format"]
    assert_equal "createOrder", replay["operation"]
    assert_equal 3, replay["events"].length
    # Redaction happened before anything left the process boundary.
    assert_equal "widget", replay["events"][0]["input"]["body"]["item"]
  end

  def test_healthy_operations_ship_backend_frames_without_a_finding
    batch = batch_for(201, true)
    frames = batch["frames"]
    assert_equal 3, frames.length
    assert(frames.all? { |frame| frame["event"]["kind"] == "backend" })
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
    batch = capture.build_batch(
      [{ "operation" => "op", "status" => 500, "events" => events }]
    )
    finding = batch["frames"][-1]["event"]
    assert_equal true, finding["context"]["captureOmitted"]
    refute finding["context"].key?("reproitCapture")
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

  def test_built_batch_passes_the_protocol_mirror_validator
    # Batch-shape guarantee: round-trip the built batch through the JS mirror
    # of reproit_protocol::EventBatch::validate (sdk/test/event_batch_v1.js).
    mirror = File.expand_path("../../test/event_batch_v1.js", __dir__)
    skip "protocol mirror not present" unless File.exist?(mirror)
    script = "const {validateEventBatch}=require(process.argv[1]);" \
      "let raw='';process.stdin.on('data',(c)=>raw+=c);" \
      "process.stdin.on('end',()=>{validateEventBatch(JSON.parse(raw));" \
      "process.stdout.write('valid')});"
    [batch_for(500, false), batch_for(201, true)].each do |batch|
      out, err, status = Open3.capture3(
        "node", "-e", script, mirror, stdin_data: R.canonical_json(batch)
      )
      assert status.success?, "protocol mirror rejected the batch: " + err
      assert_equal "valid", out
    end
  end
end
