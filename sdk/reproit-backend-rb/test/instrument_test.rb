# Outbound-exchange capture tests: the Net::HTTP hook and the database
# helper must attach request AND response to the ambient trace, bounded and
# redacted, and the batch must declare `network` only when exchanges exist.
#
# Run: ruby test/instrument_test.rb  (needs: gem install --user-install webrick)

require "json"
require "minitest/autorun"
require "net/http"
require "webrick"

require_relative "../lib/reproit_backend_rb"

class InstrumentTest < Minitest::Test
  R = ReproitBackendRb

  def setup
    R::Instrument.install
  end

  def start_upstream(&handler)
    server = WEBrick::HTTPServer.new(
      BindAddress: "127.0.0.1", Port: 0,
      Logger: WEBrick::Log.new(File::NULL), AccessLog: []
    )
    server.mount_proc("/", &handler)
    Thread.new { server.start }
    [server, server.config[:Port]]
  end

  def begin_trace
    context = {
      "trace_id" => "cap-x-1", "actor" => nil, "action_index" => 0,
      "build" => nil, "config_contract" => nil, "capture_envelope" => true
    }
    R::BackendTrace.begin(context, "GET /quote", input: { "query" => { "symbol" => "ACME" } })
  end

  def exchanges(trace)
    trace.events.filter_map { |event| event["exchange"] }
  end

  def test_net_http_records_request_and_response_on_the_ambient_trace
    server, port = start_upstream do |_request, response|
      response.status = 200
      response["content-type"] = "application/json"
      response.body = JSON.generate({ "prices" => [1, 2], "apiKey" => "sk-live-secret" })
    end
    trace = begin_trace
    R::Instrument.with_trace(trace) do
      Net::HTTP.get_response(URI("http://127.0.0.1:#{port}/prices?tier=gold"))
    end
    server.shutdown
    exchange = exchanges(trace).first
    refute_nil exchange, "exchange recorded"
    assert_equal "http", exchange["protocol"]
    assert_equal "GET", exchange["request"]["method"]
    assert_equal 200, exchange["response"]["status"]
    assert_equal [1, 2], exchange["response"]["body"]["prices"]
    # Structural redaction applies INSIDE captured exchange bodies.
    assert_equal true, exchange["response"]["body"]["apiKey"]["$reproit"]["redacted"]
  end

  def test_post_bodies_are_recorded_on_both_sides
    server, port = start_upstream do |request, response|
      response.status = 502
      response["content-type"] = "application/json"
      response.body = JSON.generate({ "error" => "upstream down", "echo" => request.body })
    end
    trace = begin_trace
    R::Instrument.with_trace(trace) do
      uri = URI("http://127.0.0.1:#{port}/convert")
      Net::HTTP.post(uri, JSON.generate({ "amount" => 5 }), "content-type" => "application/json")
    end
    server.shutdown
    exchange = exchanges(trace).first
    assert_equal "POST", exchange["request"]["method"]
    assert_equal({ "amount" => 5 }, exchange["request"]["body"])
    assert_equal 502, exchange["response"]["status"]
    assert_equal "upstream down", exchange["response"]["body"]["error"]
  end

  def test_oversized_bodies_keep_provable_identity_only
    big = "x" * (R::Exchange::MAX_EXCHANGE_BODY_BYTES + 1)
    server, port = start_upstream do |_request, response|
      response.status = 200
      response["content-type"] = "text/plain"
      response.body = big
    end
    trace = begin_trace
    R::Instrument.with_trace(trace) do
      Net::HTTP.get_response(URI("http://127.0.0.1:#{port}/blob"))
    end
    server.shutdown
    response = exchanges(trace).first["response"]
    assert_equal true, response["truncated"]
    assert_equal big.bytesize, response["bodyBytes"]
    assert_match(/\A[0-9a-f]{64}\z/, response["bodySha256"])
    assert_nil response["body"]
  end

  def test_database_helper_records_rows_and_errors
    trace = begin_trace
    R::Instrument.with_trace(trace) do
      R::Instrument.db("SELECT id FROM issuers WHERE symbol = $1", ["ACME"]) do
        { "command" => "SELECT", "rowCount" => 1, "rows" => [{ "id" => 7 }] }
      end
      begin
        R::Instrument.db("SELECT boom") { raise "relation missing" }
      rescue RuntimeError
        nil
      end
    end
    recorded = exchanges(trace)
    assert_equal 2, recorded.length
    assert_equal "db", recorded[0]["protocol"]
    assert_equal ["ACME"], recorded[0]["request"]["values"]
    assert_equal [{ "id" => 7 }], recorded[0]["response"]["rows"]
    assert_equal "relation missing", recorded[1]["response"]["error"]["message"]
  end

  def test_batch_declares_network_only_when_exchanges_exist
    handle = R::Capture.create(
      endpoint: "http://c/v1/capture-batches", api_key: "sk", app_id: "app-demo"
    )
    trace = R::BackendTrace.begin(handle.context, "GET /quote")
    trace.effect("call", resource: "pricing", key: "GET /prices", exchange: {
      "protocol" => "http",
      "request" => { "method" => "GET", "url" => "http://pricing/prices" },
      "response" => { "status" => 200, "body" => { "prices" => nil } },
    })
    trace.finish({ "error" => "boom" }, 500, false, true)
    batch = handle.build_batch(
      [{ "operation" => "GET /quote", "status" => 500, "events" => trace.events.dup }]
    )
    network = batch["capabilities"].find { |entry| entry["capability"] == "network" }
    refute_nil network, "network capability declared"
    assert_equal "complete", network["completeness"]

    plain = R::BackendTrace.begin(handle.context, "GET /quote")
    plain.effect("read", resource: "inventory", key: "widget")
    plain.finish(nil, 500, false, true)
    bare = handle.build_batch(
      [{ "operation" => "GET /quote", "status" => 500, "events" => plain.events.dup }]
    )
    assert_nil bare["capabilities"].find { |entry| entry["capability"] == "network" }
  end

  def test_capture_mode_stamps_the_determinism_envelope_on_events
    handle = R::Capture.create(
      endpoint: "http://c/v1/capture-batches", api_key: "sk", app_id: "app-demo"
    )
    capture_trace = R::BackendTrace.begin(handle.context, "op")
    assert capture_trace.events.all? { |event| event["at"].is_a?(Integer) }
    assert capture_trace.events.all? { |event| event["monoNs"].is_a?(Integer) }
    # Scan-time traces stay byte-stable: no envelope stamps.
    scan = R::BackendTrace.begin(
      { "trace_id" => "trace-a", "actor" => nil, "action_index" => 0,
        "build" => nil, "config_contract" => nil }, "op"
    )
    assert scan.events.none? { |event| event.key?("at") || event.key?("monoNs") }
  end
end
