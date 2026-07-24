# Experimental Reproit backend adapter for Ruby (Rack: Rails, Sinatra).
#
# Ruby port of sdk/reproit-backend-rs: a scan-time trace adapter that is inert
# without `x-reproit-trace`, plus an off-by-default production capture mode.

require_relative "reproit_backend_rb/trace"
require_relative "reproit_backend_rb/capture"
require_relative "reproit_backend_rb/rack"
