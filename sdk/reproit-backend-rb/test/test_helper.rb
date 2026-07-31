# frozen_string_literal: true

# Keep the capture tests hermetic against the CI environment.
#
# Capture.resolve_commit deliberately falls back to REPROIT_COMMIT and then
# GITHUB_SHA, which is correct behavior: a deployment should carry its code
# identity without being told twice. But it means a test asserting an exact
# `deployment` shape passes on a laptop and fails on a GitHub runner, where
# GITHUB_SHA is always set.
#
# This is the same defect the Python SDK hit (fixed by tests/conftest.py) and
# the Java SDK hit (fixed by a replaceable Capture.environment). It surfaced in
# each language separately, and in Ruby's case only once SDK support tiers were
# abolished and this suite began gating a release. The shared lesson is that a
# suite must STATE the environment it needs rather than inherit it.
#
# The fallback itself is proven on purpose, not by accident, in capture_test.rb.
module AmbientCodeIdentity
  VARIABLES = %w[REPROIT_COMMIT GITHUB_SHA].freeze

  def before_setup
    super
    @saved_code_identity = VARIABLES.to_h { |name| [name, ENV.fetch(name, nil)] }
    VARIABLES.each { |name| ENV.delete(name) }
  end

  def after_teardown
    @saved_code_identity.each { |name, value| value.nil? ? ENV.delete(name) : ENV[name] = value }
    super
  end
end
