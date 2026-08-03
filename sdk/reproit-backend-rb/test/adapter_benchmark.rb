# Real WEBrick/Rack middleware and bounded per-dependency capture benchmark.
require "json"
require "net/http"
require "stringio"
require "webrick"
require_relative "../lib/reproit_backend_rb"

DEPENDENCIES = 64
R = ReproitBackendRb

class BenchmarkRackBridge < WEBrick::HTTPServlet::AbstractServlet
  def initialize(server, app)
    super(server)
    @app = app
  end

  def service(request, response)
    env = {
      "REQUEST_METHOD" => request.request_method, "SCRIPT_NAME" => "",
      "PATH_INFO" => request.path, "QUERY_STRING" => request.query_string || "",
      "SERVER_NAME" => "127.0.0.1", "SERVER_PORT" => request.port.to_s,
      "SERVER_PROTOCOL" => "HTTP/1.1", "rack.url_scheme" => "http",
      "rack.input" => StringIO.new((request.body || "").b), "rack.errors" => $stderr,
    }
    request.each { |name, value| env["HTTP_" + name.upcase.tr("-", "_")] = value }
    status, headers, body = @app.call(env)
    response.status = status.to_i
    headers.each { |name, value| response[name] = Array(value).join(", ") }
    response.body = body.each_with_object("") { |part, all| all << part }
    body.close if body.respond_to?(:close)
  end
end

def configured(name, fallback)
  value = ENV[name].to_i
  value.positive? ? value : fallback
end

def median(values)
  values.sort[values.length / 2]
end

def http_cost(mounted, traced, runs)
  inner = lambda do |_env|
    [200, { "content-type" => "application/json" }, ['{"account":{"id":42,"ok":true}}']]
  end
  app = mounted ? R::Middleware.new(inner) : inner
  server = WEBrick::HTTPServer.new(
    BindAddress: "127.0.0.1", Port: 0,
    Logger: WEBrick::Log.new(File::NULL), AccessLog: []
  )
  server.mount("/", BenchmarkRackBridge, app)
  thread = Thread.new { server.start }
  Net::HTTP.start("127.0.0.1", server.config[:Port], open_timeout: 5, read_timeout: 5) do |http|
    fire = lambda do
      request = Net::HTTP::Get.new("/account?id=42")
      request["x-reproit-trace"] = "bench-trace" if traced
      response = http.request(request)
      raise "benchmark HTTP #{response.code}" unless response.code.to_i == 200
    end
    [500, runs / 4].min.times { fire.call }
    started = Process.clock_gettime(Process::CLOCK_MONOTONIC)
    runs.times { fire.call }
    return (Process.clock_gettime(Process::CLOCK_MONOTONIC) - started) * 1_000_000 / runs
  end
ensure
  server&.shutdown
  thread&.join
end

def dependency_cost(captured, runs)
  context = { "trace_id" => "dependency-benchmark", "action_index" => 1 }
  exchange = {
    "request" => { "method" => "GET", "url" => "http://pricing.test/quote?tier=gold" },
    "response" => { "status" => 200, "body" => { "price" => 42 } },
  }
  started = Process.clock_gettime(Process::CLOCK_MONOTONIC)
  runs.times do
    trace = R::BackendTrace.begin(context, "dependencyBenchmark")
    next unless captured
    DEPENDENCIES.times do |index|
      trace.effect("call", resource: "pricing", key: index.to_s, exchange: exchange)
    end
  end
  (Process.clock_gettime(Process::CLOCK_MONOTONIC) - started) * 1_000_000 /
    (runs * DEPENDENCIES)
end

runs = configured("REPROIT_ADAPTER_BENCH_RUNS", 1_000)
rounds = configured("REPROIT_ADAPTER_BENCH_ROUNDS", 5)
samples = Hash.new { |hash, key| hash[key] = [] }
dependencies = Hash.new { |hash, key| hash[key] = [] }
rounds.times do
  samples[:baseline] << http_cost(false, false, runs)
  samples[:inactive] << http_cost(true, false, runs)
  samples[:active] << http_cost(true, true, runs)
  samples[:control] << http_cost(false, false, runs)
  dependencies[:baseline] << dependency_cost(false, runs)
  dependencies[:captured] << dependency_cost(true, runs)
  dependencies[:control] << dependency_cost(false, runs)
end
baseline = median(samples[:baseline])
noise = (median(samples[:control]) - baseline).abs
inactive = median(samples[:inactive]) - baseline
active = median(samples[:active]) - baseline
dependency_baseline = median(dependencies[:baseline])
dependency_noise = (median(dependencies[:control]) - dependency_baseline).abs
dependency_capture = median(dependencies[:captured]) - dependency_baseline
raise "Ruby HTTP noise #{noise}us" unless noise < 500
raise "Ruby inactive cost #{inactive}us" unless inactive < 500
raise "Ruby active cost #{active}us" unless active < 1_500
raise "Ruby dependency noise #{dependency_noise}us" unless dependency_noise < 20
raise "Ruby dependency cost #{dependency_capture}us" unless dependency_capture < 100
puts JSON.generate(
  language: "ruby", runs: runs, rounds: rounds,
  noiseFloorMicros: noise.round(2), baselineMicros: baseline.round(2),
  inactiveCostMicros: inactive.round(2), activeCostMicros: active.round(2),
  dependencyNoiseFloorMicros: dependency_noise.round(2),
  dependencyCaptureCostMicros: dependency_capture.round(2), dependencyCeilingMicros: 100
)
