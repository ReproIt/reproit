# Experimental Reproit backend adapter for Ruby (Rack: Rails, Sinatra).
#
# Ruby port of sdk/reproit-backend-rs: a scan-time trace adapter that is inert
# without `x-reproit-trace`, plus an off-by-default production capture mode.

require_relative "reproit_backend_rb/trace"
require_relative "reproit_backend_rb/exchange"
require_relative "reproit_backend_rb/replay"
require_relative "reproit_backend_rb/instrument"
require_relative "reproit_backend_rb/pg_client"
require_relative "reproit_backend_rb/capture"
require_relative "reproit_backend_rb/rack"

module ReproitBackendRb
  # CI capture mode (ci.rb): test-triggered capsules for the flaky-CI wedge.
  # Autoloaded so requiring the SDK never loads minitest/autorun (whose
  # at_exit runner no production host should inherit).
  autoload :CI, File.expand_path("reproit_backend_rb/ci", __dir__)
end
