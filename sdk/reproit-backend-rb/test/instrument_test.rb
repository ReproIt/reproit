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

  def test_streaming_bodies_record_chunk_boundaries_via_the_tee
    # SSE, the LLM shape: the app consumes the body with read_body and a
    # block; recording tees those exact yields, so the observed chunk
    # boundaries land on the exchange and the exchange lands at EOF.
    parts = ["data: a\n\n", "data: b\n\n", "data: c\n\n"]
    server, port = start_upstream do |_request, response|
      response.status = 200
      response["content-type"] = "text/event-stream"
      response.chunked = true
      response.body = proc do |out|
        parts.each do |part|
          out.write(part)
          out.flush if out.respond_to?(:flush)
          sleep 0.05
        end
      end
    end
    trace = begin_trace
    seen = []
    R::Instrument.with_trace(trace) do
      Net::HTTP.start("127.0.0.1", port) do |http|
        http.request(Net::HTTP::Get.new("/stream")) do |response|
          response.read_body { |chunk| seen << chunk }
        end
      end
    end
    server.shutdown
    exchange = exchanges(trace).first
    refute_nil exchange, "streamed exchange recorded"
    response = exchange["response"]
    assert_equal parts.join, response["body"]
    stream = response["stream"]
    refute_nil stream, "stream boundaries recorded"
    assert_equal seen.map(&:bytesize), stream["chunks"],
                 "boundaries must be the chunks the app itself observed"
    assert_operator stream["chunks"].length, :>=, 2, "the tee must not drain in one gulp"
  end

  def test_abandoned_stream_records_after_net_http_drains_it
    server, port = start_upstream do |_request, response|
      response.status = 200
      response["content-type"] = "application/json"
      response.body = JSON.generate({ "ok" => true })
    end
    trace = begin_trace
    R::Instrument.with_trace(trace) do
      Net::HTTP.start("127.0.0.1", port) do |http|
        # The caller's block never reads the body; Net::HTTP drains it after
        # the block, so the exchange still records (as one chunk).
        http.request(Net::HTTP::Get.new("/ignored")) { |_response| nil }
      end
    end
    server.shutdown
    exchange = exchanges(trace).first
    refute_nil exchange
    assert_equal({ "ok" => true }, exchange["response"]["body"])
  end

  FakeResult = Struct.new(:rows_array, :status, :count) do
    def to_a
      rows_array
    end

    def cmd_status
      status
    end

    def cmd_tuples
      count
    end
  end

  def fake_pg
    Module.new do
      const_set(:Connection, Class.new do
        def self.connect(*)
          new
        end

        def exec_params(text, _values = nil)
          raise "relation missing" if text.include?("boom")
          InstrumentTest::FakeResult.new([{ "id" => 7, "symbol" => "ACME" }], "SELECT 1", 1)
        end
      end)
    end
  end

  def test_wrapped_pg_records_statements_in_the_node_wire_shape
    pg = R.wrap_pg(fake_pg)
    connection = pg.const_get(:Connection).connect
    trace = begin_trace
    R::Instrument.with_trace(trace) do
      connection.exec_params("SELECT id, symbol FROM issuers WHERE symbol = $1", ["ACME"])
      begin
        connection.exec_params("SELECT boom")
      rescue RuntimeError
        nil
      end
    end
    recorded = exchanges(trace)
    assert_equal 2, recorded.length
    assert_equal "pg", recorded[0]["protocol"]
    assert_equal ["ACME"], recorded[0]["request"]["values"]
    assert_equal "SELECT", recorded[0]["response"]["command"]
    assert_equal 1, recorded[0]["response"]["rowCount"]
    assert_equal [{ "id" => 7, "symbol" => "ACME" }], recorded[0]["response"]["rows"]
    assert_equal "relation missing", recorded[1]["response"]["error"]["message"]
  end

  def test_wrapping_pg_twice_does_not_double_record
    pg = fake_pg
    R.wrap_pg(pg)
    R.wrap_pg(pg)
    trace = begin_trace
    R::Instrument.with_trace(trace) do
      pg.const_get(:Connection).connect.exec_params("SELECT 1")
    end
    assert_equal 1, exchanges(trace).length
  end

  def test_over_cap_exchanges_drop_with_the_counter_and_spare_the_request
    trace = begin_trace
    before = R::Instrument.stats[:failed_captures]
    R::Instrument.with_trace(trace) do
      # Fill the trace to its event cap, then one more exchange: the drop
      # must be counted and must never surface into the host request.
      (R::MAX_EVENTS - trace.events.length).times do |index|
        trace.effect("read", resource: "fill", key: index.to_s)
      end
      R::Instrument.record("call", "svc", "GET /over", { "protocol" => "http" })
    end
    assert_equal R::MAX_EVENTS, trace.events.length
    assert_equal before + 1, R::Instrument.stats[:failed_captures]
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
