# Experimental, framework-neutral backend instrumentation.
#
# Ruby port of sdk/reproit-backend-rs/src/lib.rs. Scan-time: services activate
# this adapter only when a trusted request carries `x-reproit-trace`. The
# resulting response header (`x-reproit-events`) contains bounded, trace-bound,
# structurally redacted events. Production: the optional, config-gated capture
# mode (capture.rb) self-samples finished traces. It is not a public
# compatibility surface while backend contracts remain experimental.
#
# Wire parity with the Rust adapter: events serialize as compact JSON with
# recursively sorted keys (serde_json's BTreeMap order), and the header is
# unpadded base64url of that encoding (Array#pack, no base64 gem dependency).

require "digest"
require "json"

module ReproitBackendRb
  MAX_EVENTS = 256
  MAX_HEADER_BYTES = 60_000
  EFFECT_KINDS = %w[read write delete emit call].freeze

  PATH_SEGMENT = /\A[A-Za-z_][A-Za-z0-9_]*\z/
  SECRET_PARTS = %w[
    password passwd secret token authorization cookie email phone apikey
    publishablekey privatekey accesskey signingkey idempotencykey
  ].freeze

  # Codes: InvalidOperation, AlreadyFinished, TooManyEvents, HeaderTooLarge.
  class TraceError < StandardError
    attr_reader :code

    def initialize(code)
      super("reproit trace rejected input: " + code)
      @code = code
    end
  end

  @sequence_lock = Mutex.new
  @sequence = 0

  def self.next_sequence
    @sequence_lock.synchronize { @sequence += 1 }
  end

  def self.bounded(value, maximum)
    return nil unless value.is_a?(String)
    value = value.strip
    return nil if value.empty? || value.length > maximum
    value
  end

  # `get.call(name)` returns the request header value (or nil). Returns nil
  # when no valid `x-reproit-trace` is present: the adapter stays inert.
  def self.trace_context_from_headers(get)
    trace_id = bounded(get.call("x-reproit-trace"), 128)
    return nil if trace_id.nil?
    raw_action = get.call("x-reproit-action")
    action_index = 0
    if raw_action.is_a?(String)
      parsed = Integer(raw_action.strip, 10, exception: false)
      action_index = parsed if parsed && parsed >= 0 && parsed <= 0xFFFFFFFF
    end
    {
      "trace_id" => trace_id,
      "actor" => bounded(get.call("x-reproit-actor"), 32),
      "action_index" => action_index,
      "build" => bounded(get.call("x-reproit-build"), 128),
      "config_contract" => bounded(get.call("x-reproit-config-contract"), 128),
    }
  end

  def self.valid_path?(path)
    return false unless path.is_a?(String) && !path.empty?
    path.split(".", -1).all? do |segment|
      name = segment.end_with?("[]") ? segment[0..-3] : segment
      PATH_SEGMENT.match?(name)
    end
  end

  # GraphQL selection mapping (parser-produced only); nil when invalid.
  def self.selection(schema_path, response_path, type_condition = nil)
    return nil unless valid_path?(schema_path) && valid_path?(response_path)
    value = { "schemaPath" => schema_path, "responsePath" => response_path }
    unless type_condition.nil?
      invalid = !valid_path?(type_condition) || type_condition.include?(".") ||
        type_condition.include?("[]")
      return nil if invalid
      value["typeCondition"] = type_condition
    end
    value
  end

  # Canonical decoded OpenAPI input. Framework adapters must provide decoded
  # values (including arrays for repeated query/header parameters), never raw
  # query strings whose serialization style is ambiguous.
  def self.http_input(body: nil, path: nil, query: nil, headers: nil)
    value = {}
    value["body"] = body unless body.nil?
    { "path" => path, "query" => query, "headers" => headers }.each do |name, fields|
      next if fields.nil? || fields.empty?
      value[name] = fields.each_with_object({}) do |(key, field), folded|
        folded[name == "headers" ? key.to_s.downcase : key.to_s] = field
      end
    end
    value
  end

  # Compact JSON with recursively sorted keys: byte-identical to the Rust
  # adapter's serde_json (BTreeMap) encoding of the same events.
  def self.canonical_json(value)
    case value
    when nil then "null"
    when Hash
      body = value.keys.sort_by(&:to_s).map do |key|
        JSON.generate(key.to_s) + ":" + canonical_json(value[key])
      end
      "{" + body.join(",") + "}"
    when Array then "[" + value.map { |item| canonical_json(item) }.join(",") + "]"
    when Symbol then JSON.generate(value.to_s)
    else JSON.generate(value)
    end
  end

  def self.identity(value)
    "sha256:" + Digest::SHA256.digest(value)[0, 12].unpack1("H*")
  end

  def self.secret_field?(name)
    folded = name.gsub(/[^A-Za-z0-9]/, "").downcase
    SECRET_PARTS.any? { |part| folded.include?(part) }
  end

  # Recursive structural redaction: secret-named fields become `$reproit`
  # metadata stubs (type + length), everything else recurses.
  def self.redact(value)
    case value
    when Hash
      value.each_with_object({}) do |(key, field), folded|
        folded[key] = secret_field?(key.to_s) ? metadata(field) : redact(field)
      end
    when Array then value.map { |item| redact(item) }
    else value
    end
  end

  def self.metadata(value)
    kind, length = "null", nil
    case value
    when true, false then kind = "boolean"
    when Integer then kind = "integer"
    when Float then kind = "number"
    when String, Symbol then kind, length = "string", value.to_s.length
    when Array then kind, length = "array", value.length
    when Hash then kind = "object"
    end
    { "$reproit" => { "redacted" => true, "type" => kind, "length" => length } }
  end

  # One traced operation: a start event, observed effects, one return.
  class BackendTrace
    def initialize(common)
      @common = common
      @events = []
      @finished = false
    end

    def self.begin(context, operation, span_id: nil, tenant: nil, idempotency_key: nil,
                   input: nil, selections: nil)
      name = ReproitBackendRb.bounded(operation.to_s, 256)
      raise TraceError, "InvalidOperation" if name.nil?
      span = ReproitBackendRb.bounded(
        (span_id || context["trace_id"] + ":" + name).to_s, 128
      )
      raise TraceError, "InvalidOperation" if span.nil?
      common = {
        "traceId" => context["trace_id"],
        "spanId" => span,
        "actionIndex" => context["action_index"],
        "operation" => name,
      }
      common["actor"] = context["actor"] if context["actor"]
      common["build"] = context["build"] if context["build"]
      common["configContract"] = context["config_contract"] if context["config_contract"]
      unless tenant.nil?
        bounded_tenant = ReproitBackendRb.bounded(tenant.to_s, 128)
        common["tenant"] = bounded_tenant unless bounded_tenant.nil?
      end
      unless idempotency_key.nil?
        common["idempotencyKey"] = ReproitBackendRb.identity(idempotency_key.to_s)
      end
      if selections && !selections.empty?
        common["selections"] = selections.take(MAX_EVENTS)
      end
      trace = new(common)
      trace.push("start", { "input" => ReproitBackendRb.redact(input) })
      trace
    end

    def effect(kind, resource: nil, key: nil, tenant: nil, event: nil, detail: nil)
      raise TraceError, "AlreadyFinished" if @finished
      raise TraceError, "InvalidOperation" unless EFFECT_KINDS.include?(kind)
      fields = { "effect" => kind }
      { "resource" => resource, "key" => key, "effectTenant" => tenant,
        "event" => event }.each do |name, value|
        fields[name] = value.to_s[0, 256] unless value.nil?
      end
      unless detail.nil?
        redacted = ReproitBackendRb.redact(detail)
        if redacted.is_a?(Hash)
          %w[before after payload].each do |field|
            fields[field] = redacted[field] if redacted.key?(field)
          end
        end
      end
      push("effect", fields)
    end

    def finish(output, status, success, effects_complete)
      raise TraceError, "AlreadyFinished" if @finished
      push("return", {
        "output" => ReproitBackendRb.redact(output),
        "status" => status,
        "success" => success == true,
        "effectsComplete" => effects_complete == true,
      })
      @finished = true
    end

    def header
      raise TraceError, "AlreadyFinished" unless @finished
      raw = ReproitBackendRb.canonical_json(@events)
      encoded = [raw].pack("m0").tr("+/", "-_").delete("=")
      raise TraceError, "HeaderTooLarge" if encoded.length > MAX_HEADER_BYTES
      encoded
    end

    attr_reader :events

    def finished?
      @finished
    end

    def push(kind, fields)
      raise TraceError, "TooManyEvents" if @events.length >= MAX_EVENTS
      event = @common.dup
      event["sequence"] = ReproitBackendRb.next_sequence
      event["kind"] = kind
      @events << event.merge(fields)
    end
  end
end
