# Ruby hermetic acceptance fixture, mirroring the Node and Rust money tests.
#
# The /quote operation 500s because an upstream pricing service returns
# {"prices": null} and the handler indexes into it.
#
# MODE=capture: boots the upstream plus the app, fires the failing request,
# and writes a version-2 reproit-backend-capture (exchanges + envelope) to
# CAPTURE_OUT. Default (server) mode: boots ONLY the app on $PORT; with
# REPROIT_REPLAY set the SDK serves the recorded exchanges, so no upstream
# and no database exist. FIXED=1 applies the fix.

require "json"
require "webrick"

require_relative "../lib/reproit_backend_rb"

R = ReproitBackendRb
R::Instrument.install

UPSTREAM_PORT = 19_981

# A database stand-in that must never be reached for real: in capture mode a
# canned result stands in for a live driver; in replay mode the SDK serves
# the recorded exchange before this block ever runs.
def load_issuer(symbol)
  R::Instrument.db("SELECT id, symbol FROM issuers WHERE symbol = $1", [symbol]) do
    if ENV["MODE"] != "capture"
      raise "live database reached during hermetic replay"
    end
    { "command" => "SELECT", "rowCount" => 1, "rows" => [{ "id" => 7, "symbol" => symbol }] }
  end
end

def quote(symbol)
  load_issuer(symbol)
  response = Net::HTTP.get_response(URI("http://127.0.0.1:#{UPSTREAM_PORT}/prices?tier=gold"))
  body = JSON.parse(response.body)
  prices = body["prices"]
  if ENV["FIXED"] == "1" && !prices.is_a?(Array)
    return [200, { "first" => nil, "note" => "no prices available" }]
  end
  [200, { "first" => prices[0] }]
rescue StandardError
  [500, { "error" => "internal" }]
end

# Minimal Rack app so the middleware under test is the real one. Only
# /quote runs the handler: a readiness probe on any other path must not
# consume recorded exchanges, which would diverge the replay before the
# request under test even arrives.
APP = lambda do |env|
  unless env["PATH_INFO"] == "/quote"
    return [404, { "content-type" => "application/json" }, ['{"error":"not found"}']]
  end
  trace = env[R::Middleware::ENV_KEY]
  symbol = URI.decode_www_form(env["QUERY_STRING"].to_s).to_h["symbol"].to_s
  status, output = quote(symbol)
  trace&.effect("read", resource: "quote", key: symbol)
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
# uploading, so the acceptance script needs no cloud.
class FileCapture
  def context
    {
      "trace_id" => "cap-ruby-1", "actor" => nil, "action_index" => 0,
      "build" => "ruby-fixture", "config_contract" => nil, "capture_envelope" => true
    }
  end

  def record(trace)
    payload = {
      "format" => "reproit-backend-capture",
      "version" => 2,
      "operation" => trace.events[0]["operation"],
      "oracle" => "backend-server-error",
      "envelope" => {
        "observedAtMs" => (Time.now.to_f * 1000).to_i,
        "tz" => Time.now.zone.to_s,
        "runtime" => "ruby #{RUBY_VERSION}",
        "replaySeed" => "c0ffee00c0ffee00",
      },
      "events" => trace.events,
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
  server = serve(19_980, app)
  Thread.new { server.start }
  sleep 0.3
  result = Net::HTTP.get_response(URI("http://127.0.0.1:19980/quote?symbol=ACME"))
  warn "capture fixture status #{result.code}"
  sleep 0.2
  server.shutdown
  upstream.shutdown
else
  app = R::Middleware.new(APP)
  port = Integer(ENV.fetch("PORT", "19980"))
  server = serve(port, app)
  trap("INT") { server.shutdown }
  trap("TERM") { server.shutdown }
  server.start
end
