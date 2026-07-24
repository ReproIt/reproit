Gem::Specification.new do |spec|
  spec.name = "reproit-backend-rb"
  spec.version = "0.0.0"
  spec.summary =
    "Experimental trace-bound service instrumentation for Reproit (Rack, Rails, Sinatra)"
  spec.description =
    "Internal validation surface, not a published compatibility API. Scan-time trace " \
    "adapter (inert without x-reproit-trace) plus an off-by-default production capture " \
    "mode. Pure stdlib: no runtime dependency, not even on the rack gem."
  spec.authors = ["ReproIt, Inc."]
  spec.license = "Apache-2.0"
  spec.homepage = "https://reproit.com"
  spec.metadata = {
    # Private, like the rest of the SDK family (`"private": true` in Node,
    # version 0.0.0 everywhere): gem push is disabled by pinning an
    # unreachable push host.
    "allowed_push_host" => "https://gems.invalid",
    "source_code_uri" => "https://github.com/reproit/reproit-cli",
  }
  spec.required_ruby_version = ">= 3.2"
  spec.files = Dir["lib/**/*.rb"] + ["README.md"]
  spec.require_paths = ["lib"]
end
