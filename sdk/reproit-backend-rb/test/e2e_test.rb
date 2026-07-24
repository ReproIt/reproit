# Functional end-to-end test: a real Rack app served by WEBrick with a planted
# 500, real HTTP requests via net/http, and a local stub ingest server.
# Asserts the finding batch arrives correctly tagged with the reproitCapture
# sequence, and that a scan-time request round-trips x-reproit-events.
#
# Run: ruby test/e2e_test.rb  (needs: gem install --user-install webrick rack)

require "json"
require "minitest/autorun"
require "net/http"
require "open3"
require "stringio"
require "webrick"

require "rack"

require_relative "../lib/reproit_backend_rb"

# Minimal WEBrick-to-Rack bridge so the e2e stack stays stdlib plus the rack
# gem (used only for Rack::Lint, which pins the middleware to the Rack SPEC).
class RackBridge < WEBrick::HTTPServlet::AbstractServlet
  def initialize(server, app)
    super(server)
    @app = app
  end

  def service(request, response)
    env = {
      "REQUEST_METHOD" => request.request_method,
      "SCRIPT_NAME" => "",
      "PATH_INFO" => request.path,
      "QUERY_STRING" => request.query_string || "",
      "SERVER_NAME" => "127.0.0.1",
      "SERVER_PORT" => request.port.to_s,
      "SERVER_PROTOCOL" => "HTTP/1.1",
      "rack.url_scheme" => "http",
      "rack.input" => StringIO.new((request.body || "").b),
      "rack.errors" => $stderr,
    }
    request.each do |name, value|
      env["HTTP_" + name.upcase.tr("-", "_")] = value
    end
    env["CONTENT_TYPE"] = request["content-type"] if request["content-type"]
    env["CONTENT_LENGTH"] = request["content-length"] if request["content-length"]
    env.delete("HTTP_CONTENT_TYPE")
    env.delete("HTTP_CONTENT_LENGTH")
    status, headers, body = @app.call(env)
    response.status = status.to_i
    headers.each do |key, value|
      response[key] = value.is_a?(Array) ? value.join(", ") : value
    end
    parts = +""
    body.each { |part| parts << part }
    body.close if body.respond_to?(:close)
    response.body = parts
  end
end

class E2eTest < Minitest::Test
  R = ReproitBackendRb

  def start_stub_ingest(received)
    server = WEBrick::HTTPServer.new(
      BindAddress: "127.0.0.1", Port: 0,
      Logger: WEBrick::Log.new(File::NULL), AccessLog: []
    )
    server.mount_proc("/v1/events") do |request, response|
      received << {
        "authorization" => request["authorization"],
        "batch" => JSON.parse(request.body),
      }
      response.status = 200
      response["content-type"] = "application/json"
      response.body = '{"accepted":true}'
    end
    Thread.new { server.start }
    [server, "http://127.0.0.1:#{server.config[:Port]}/v1/events"]
  end

  def app(capture)
    inner = lambda do |env|
      case [env["REQUEST_METHOD"], env["PATH_INFO"]]
      when ["GET", "/ok"]
        [200, { "content-type" => "application/json" }, ['{"ok":true}']]
      when ["POST", "/boom"]
        trace = env["reproit.trace"]
        trace&.effect("write", resource: "orders", key: "1")
        [500, { "content-type" => "application/json" }, ['{"error":"boom"}']]
      else
        [404, { "content-type" => "application/json" }, ['{"error":"not found"}']]
      end
    end
    Rack::Lint.new(R::Middleware.new(Rack::Lint.new(inner), capture: capture))
  end

  def request(url, method: "GET", body: nil, headers: {})
    uri = URI.parse(url)
    http = Net::HTTP.new(uri.host, uri.port)
    http.open_timeout = 5
    http.read_timeout = 5
    klass = method == "POST" ? Net::HTTP::Post : Net::HTTP::Get
    req = klass.new(uri.request_uri)
    headers.each { |name, value| req[name] = value }
    req.body = body unless body.nil?
    http.request(req)
  end

  def decode_header(header)
    padded = header + "=" * ((4 - header.length % 4) % 4)
    JSON.parse(padded.tr("-_", "+/").unpack1("m0"))
  end

  def validate_with_protocol_mirror(batch)
    mirror = File.expand_path("../../test/event_batch_v1.js", __dir__)
    skip "protocol mirror not present" unless File.exist?(mirror)
    script = "const {validateEventBatch}=require(process.argv[1]);" \
      "let raw='';process.stdin.on('data',(c)=>raw+=c);" \
      "process.stdin.on('end',()=>{validateEventBatch(JSON.parse(raw));" \
      "process.stdout.write('valid')});"
    out, err, status = Open3.capture3(
      "node", "-e", script, mirror, stdin_data: JSON.generate(batch)
    )
    assert status.success?, "protocol mirror rejected the batch: " + err
    assert_equal "valid", out
  end

  def test_rack_planted_500_ships_a_tagged_finding_batch
    received = []
    ingest, ingest_url = start_stub_ingest(received)
    capture = R::Capture.create(
      endpoint: ingest_url, api_key: "sk_live_test", app_id: "app-e2e",
      build: "9.9.9", flush_interval_ms: 100
    )
    refute_nil capture
    server = WEBrick::HTTPServer.new(
      BindAddress: "127.0.0.1", Port: 0,
      Logger: WEBrick::Log.new(File::NULL), AccessLog: []
    )
    server.mount("/", RackBridge, app(capture))
    Thread.new { server.start }
    base = "http://127.0.0.1:#{server.config[:Port]}"

    begin
      boom = request(
        base + "/boom", method: "POST",
        body: JSON.generate({ "item" => "widget", "apiKey" => "sk_live_leak" }),
        headers: { "content-type" => "application/json" }
      )
      assert_equal 500, boom.code.to_i
      assert_equal true, capture.flush(5.0)

      assert_equal 1, received.length
      assert_equal "Bearer sk_live_test", received[0]["authorization"]
      batch = received[0]["batch"]
      validate_with_protocol_mirror(batch)
      assert_equal 1, batch["version"]
      assert_equal "app-e2e", batch["appId"]
      assert_equal({ "version" => "9.9.9" }, batch["deployment"])
      findings = batch["frames"].map { |f| f["event"] }.select { |e| e["kind"] == "finding" }
      assert_equal 1, findings.length
      finding = findings[0]
      assert_equal R::SERVER_ERROR_ORACLE, finding["identity"]["oracle"]
      assert_equal "reproit-backend-rb", finding["context"]["capture"]
      replay = finding["context"]["reproitCapture"]
      assert_equal R::CAPTURE_FORMAT, replay["format"]
      assert_equal R::SERVER_ERROR_ORACLE, replay["oracle"]
      assert_equal %w[start effect return], replay["events"].map { |event| event["kind"] }
      assert_equal "orders", replay["events"][1]["resource"]
      assert_equal 500, replay["events"][2]["status"]
      assert_equal false, replay["events"][2]["success"]
      # The secret-shaped input field was structurally redacted before upload.
      start = replay["events"][0]
      assert_equal true, start["input"]["body"]["apiKey"]["$reproit"]["redacted"]
      assert_equal "widget", start["input"]["body"]["item"]

      # Scan-time request: header round-trip, no capture of the healthy call.
      ok = request(
        base + "/ok",
        headers: { "x-reproit-trace" => "trace-e2e", "x-reproit-actor" => "alice" }
      )
      assert_equal 200, ok.code.to_i
      header = ok["x-reproit-events"]
      refute_nil header, "expected an x-reproit-events response header"
      events = decode_header(header)
      assert_equal "trace-e2e", events[0]["traceId"]
      assert_equal "alice", events[0]["actor"]
      assert_equal "return", events[-1]["kind"]
      assert_equal 200, events[-1]["status"]
      assert_equal 1, capture.stats[:captured_operations]
    ensure
      server.shutdown
      ingest.shutdown
    end
  end
end
