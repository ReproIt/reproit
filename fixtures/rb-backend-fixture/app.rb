# Money-test fixture for Ruby capsule parity: a Rack app (WEBrick-bridged,
# the rb SDK test idiom) with the reproit SDK whose /quote operation 500s
# because an upstream pricing service returns {"prices": null} and the
# handler indexes into it. The upstream call goes through Net::HTTP (the
# prepend hook) and the database call through a pg-shaped driver wrapped by
# `wrap_pg` (the same fake-driver idiom the Node and Python fixtures use: a
# driver that MUST never be reached during hermetic replay).
#
# MODE=capture boots the upstream plus the app, fires the failing request,
# and writes a version 2 reproit-backend-capture (exchanges plus envelope)
# to CAPTURE_OUT. Default (server) mode boots ONLY the app on $PORT; with
# REPROIT_REPLAY set the SDK serves the recorded exchanges in process, so
# neither the upstream nor the database exists. FIXED=1 applies the fix.

require "json"
require "stringio"
require "webrick"

require_relative File.join(
  "..", "..", "sdk", "reproit-backend-rb", "lib", "reproit_backend_rb"
)

R = ReproitBackendRb
R::Instrument.install

UPSTREAM_PORT = 19_983
CAPTURE_PORT = 19_984

# pg-shaped driver that MUST never be reached for real: in capture mode a
# canned result stands in for a live database; in replay mode the SDK's
# connect stub serves the recorded exchange before this class ever runs.
module FakePG
  Result = Struct.new(:rows) do
    def to_a
      rows
    end

    def first
      rows.first
    end

    def cmd_status
      "SELECT 1"
    end

    def cmd_tuples
      rows.length
    end
  end

  class Connection
    def self.connect(*)
      raise "live database dialed during hermetic replay" if ENV["MODE"] != "capture"
      new
    end

    def exec_params(_text, values)
      raise "live database reached during hermetic replay" if ENV["MODE"] != "capture"
      Result.new([{ "id" => 7, "symbol" => values[0].to_s }])
    end

    def close
      nil
    end
  end
end

PG = R.wrap_pg(FakePG)

def quote(symbol)
  connection = PG::Connection.connect("postgresql://db.internal/quotes")
  begin
    issuer = connection.exec_params(
      "SELECT id, symbol FROM issuers WHERE symbol = $1", [symbol]
    ).first
    return [404, { "error" => "unknown symbol" }] if issuer.nil?
  ensure
    connection.close
  end
  response = Net::HTTP.get_response(
    URI("http://127.0.0.1:#{UPSTREAM_PORT}/prices?tier=gold")
  )
  prices = JSON.parse(response.body)["prices"]
  if ENV["FIXED"] == "1" && !prices.is_a?(Array)
    return [200, { "first" => nil, "note" => "no prices available" }]
  end
  [200, { "first" => prices[0] }]
rescue StandardError
  [500, { "error" => "internal" }]
end

# Minimal Rack app so the middleware under test is the real one. Only /quote
# runs the handler: a readiness probe on any other path must not consume
# recorded exchanges, which would diverge the replay before the request
# under test even arrives.
APP = lambda do |env|
  unless env["PATH_INFO"] == "/quote"
    return [404, { "content-type" => "application/json" }, ['{"error":"not found"}']]
  end
  symbol = URI.decode_www_form(env["QUERY_STRING"].to_s).to_h["symbol"].to_s
  status, output = quote(symbol)
  [status, { "content-type" => "application/json" }, [JSON.generate(output)]]
end

class Bridge < WEBrick::HTTPServlet::AbstractServlet
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
      "rack.url_scheme" => "http",
      "rack.input" => StringIO.new((request.body || "").b),
      "rack.errors" => $stderr,
    }
    request.each { |name, value| env["HTTP_" + name.upcase.tr("-", "_")] = value }
    status, headers, body = @app.call(env)
    response.status = status.to_i
    headers.each { |key, value| response[key] = value }
    parts = +""
    body.each { |part| parts << part }
    response.body = parts
  end
end

# Capture sink that writes the replayable payload to disk instead of
# uploading it, so the fixture needs no cloud.
class FileCapture
  def context
    {
      "trace_id" => "cap-money-rb-fixture-1", "actor" => nil, "action_index" => 0,
      "build" => "rb-money-fixture", "config_contract" => nil, "capture_envelope" => true
    }
  end

  def record(trace)
    events = trace.events
    payload = {
      "format" => R::CAPTURE_FORMAT,
      "version" => R::CAPTURE_VERSION_EXCHANGES,
      "operation" => events[0]["operation"],
      "oracle" => R::SERVER_ERROR_ORACLE,
      "envelope" => R.determinism_envelope(events[0]["at"]),
      "events" => events,
    }
    File.write(ENV.fetch("CAPTURE_OUT"), R.canonical_json(payload))
  end
end

def serve(port, app)
  server = WEBrick::HTTPServer.new(
    BindAddress: "127.0.0.1", Port: port,
    Logger: WEBrick::Log.new(File::NULL), AccessLog: []
  )
  server.mount("/", Bridge, app)
  server
end

if ENV["MODE"] == "capture"
  upstream = WEBrick::HTTPServer.new(
    BindAddress: "127.0.0.1", Port: UPSTREAM_PORT,
    Logger: WEBrick::Log.new(File::NULL), AccessLog: []
  )
  upstream.mount_proc("/prices") do |_request, response|
    response.status = 200
    response["content-type"] = "application/json"
    response.body = JSON.generate({ "prices" => nil })
  end
  Thread.new { upstream.start }
  app = R::Middleware.new(APP, capture: FileCapture.new)
  server = serve(CAPTURE_PORT, app)
  Thread.new { server.start }
  sleep 0.3
  result = Net::HTTP.get_response(URI("http://127.0.0.1:#{CAPTURE_PORT}/quote?symbol=ACME"))
  warn "capture fixture status #{result.code}"
  sleep 0.2
  server.shutdown
  upstream.shutdown
else
  app = R::Middleware.new(APP)
  port = Integer(ENV.fetch("PORT", CAPTURE_PORT.to_s))
  server = serve(port, app)
  trap("INT") { server.shutdown }
  trap("TERM") { server.shutdown }
  server.start
end
