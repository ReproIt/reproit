# Planted order-dependent test failure that fires only under CI-like
# conditions, for the flaky-CI wedge (Track 3), Ruby port of
# fixtures/flaky-ci-fixture.
#
# The first test runs ONLY on the CI legacy matrix (CI_LEGACY_MATRIX=1) and
# leaks state into the shared config service: it switches the service to its
# legacy response format, which returns the tax rate as a string. The second
# test then errors computing the total and fails. A plain local run never
# takes the legacy branch, so the suite passes and the failure looks
# unreproducible ("flaky"). The capsule spooled by the CI run carries the
# recorded legacy response, so `reproit check <capsule> --exec "ruby
# tests/checkout_test.rb"` re-executes the exact failing run anywhere.
#
# Run it directly (`ruby tests/checkout_test.rb`): the CI suite runs tests in
# declaration order and writes the stderr markers `reproit check` parses from
# this same process.

require "json"
require "net/http"
require "socket"

require_relative "../../../sdk/reproit-backend-rb/lib/reproit_backend_rb"
require_relative "../order"

PORT = 19_992
CONFIG_URL = "http://127.0.0.1:#{PORT}".freeze

# The shared config service both tests talk to. Stateful on purpose: the
# legacy-format test leaks its toggle into it. Never started under replay,
# where the SDK serves the recorded exchanges in process and any real socket
# attempt would surface as a divergence, not a connection.
if ENV["REPROIT_REPLAY"].to_s.empty?
  legacy = false
  server = TCPServer.new("127.0.0.1", PORT)
  Thread.new do
    loop do
      client = server.accept
      begin
        request_line = client.gets.to_s
        loop { break if client.gets.to_s.strip.empty? }
        if request_line.start_with?("POST /format/legacy")
          legacy = true
          client.write("HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n")
        else
          body = JSON.generate({ "rate" => legacy ? "0.25" : 0.25 })
          client.write(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n" \
            "content-length: #{body.bytesize}\r\nconnection: close\r\n\r\n" + body
          )
        end
      ensure
        client.close
      end
    end
  end
end

t = ReproitBackendRb::CI.suite("checkout")

t.call("legacy config format toggles") do
  # CI-only: this is the state leak that makes the next test order
  # dependent. A local run never takes this branch.
  if ENV["CI_LEGACY_MATRIX"] == "1"
    response = Net::HTTP.post(URI(CONFIG_URL + "/format/legacy"), "")
    assert_equal "204", response.code
  end
end

t.call("order total applies the configured tax rate") do
  assert_equal 125, order_total(100, CONFIG_URL)
end
