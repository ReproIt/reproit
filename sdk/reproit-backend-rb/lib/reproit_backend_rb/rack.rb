# Rack middleware for Rails, Sinatra, and any other Rack 2/3 app.
#
# Scan-time: inert unless the request carries `x-reproit-trace`; the finished
# trace is returned as the `x-reproit-events` response header. Production: pass
# a Capture and every request is traced and handed to the sampler instead.
# Handlers record observed effects via `request.env["reproit.trace"]`. Every
# adapter path fails closed: instrumentation errors never reach the host app.
#
# Bodies are buffered so the start/return events carry the decoded JSON
# payloads up to a fixed cap; larger or non-JSON bodies are traced without
# content. Route parameters are matched after middleware runs, so they are not
# part of the canonical input here. Pure stdlib: the rack gem is not required.

require "json"
require "stringio"
require "uri"

require_relative "trace"

module ReproitBackendRb
  MAX_BODY_BYTES = 64 * 1024

  # Rails: config.middleware.use ReproitBackendRb::Middleware, capture: capture
  # Sinatra: use ReproitBackendRb::Middleware, capture: capture
  class Middleware
    ENV_KEY = "reproit.trace"

    def initialize(app, capture: nil, operation: nil, effects_complete: false)
      @app = app
      @capture = capture
      @operation = operation
      @effects_complete = effects_complete == true
    end

    def call(env)
      begin
        headers = request_headers(env)
        scan_context = ReproitBackendRb.trace_context_from_headers(->(name) { headers[name] })
        context = scan_context
        context = @capture.context if context.nil? && !@capture.nil?
        return @app.call(env) if context.nil?

        # Buffer the request body so the start event carries it, then hand the
        # app a rewound replacement stream.
        body = read_request_body(env)
        operation =
          if @operation.respond_to?(:call)
            @operation.call(env)
          else
            (env["REQUEST_METHOD"] || "GET") + " " + (env["PATH_INFO"] || "/")
          end
        trace = BackendTrace.begin(
          context,
          operation,
          input: ReproitBackendRb.http_input(
            body: decode_json(body, headers["content-type"] || ""),
            query: query_values(env["QUERY_STRING"] || ""),
            headers: headers
          )
        )
        env[ENV_KEY] = trace
      rescue StandardError
        # Fail closed: an instrumentation defect must not break the request.
        return @app.call(env)
      end

      status, response_headers, response_body = @app.call(env)
      begin
        unless trace.finished?
          buffered, response_body, complete = buffer_response_body(response_body)
          content_type = header_value(response_headers, "content-type") || ""
          output = complete ? decode_json(buffered, content_type) : nil
          trace.finish(output, status.to_i, status.to_i < 500, @effects_complete)
          if !scan_context.nil?
            response_headers = set_header(response_headers, "x-reproit-events", trace.header)
          elsif !@capture.nil?
            @capture.record(trace)
          end
        end
      rescue StandardError
        # Oversized or over-long traces drop their header; ship anyway.
        nil
      end
      [status, response_headers, response_body]
    end

    private

    # Lowercased hyphenated request header names from the Rack env.
    def request_headers(env)
      headers = {}
      env.each do |key, value|
        next unless value.is_a?(String)
        if key.start_with?("HTTP_")
          headers[key[5..].downcase.tr("_", "-")] = value
        elsif key == "CONTENT_TYPE"
          headers["content-type"] = value
        elsif key == "CONTENT_LENGTH"
          headers["content-length"] = value
        end
      end
      headers
    end

    def read_request_body(env)
      input = env["rack.input"]
      return "" if input.nil?
      body = input.read || ""
      body = body.dup.force_encoding(Encoding::BINARY)
      env["rack.input"] = StringIO.new(body)
      body
    end

    def decode_json(body, content_type)
      return nil if body.empty? || body.bytesize > MAX_BODY_BYTES
      return nil unless content_type.include?("application/json")
      JSON.parse(body.dup.force_encoding(Encoding::UTF_8))
    rescue JSON::ParserError, EncodingError
      nil
    end

    def query_values(query_string)
      values = {}
      URI.decode_www_form(query_string).each do |key, value|
        if values.key?(key)
          prior = values[key]
          values[key] = prior.is_a?(Array) ? prior + [value] : [prior, value]
        else
          values[key] = value
        end
      end
      values
    rescue ArgumentError
      {}
    end

    # Drain the Rack body (Array, Enumerable, or streaming) into one string so
    # the return event can carry the decoded output; hand back a replacement
    # single-part body. Bodies over the cap are traced without content.
    def buffer_response_body(body)
      buffered = +""
      complete = true
      # Rack 3 SPEC: an array-backed body must be consumed via to_ary, a
      # streaming body via each (then closed).
      if body.respond_to?(:to_ary)
        parts = body.to_ary
      elsif body.respond_to?(:each)
        parts = []
        body.each { |part| parts << part }
        body.close if body.respond_to?(:close)
      else
        return ["", body, false]
      end
      parts.each do |part|
        buffered << part.to_s
        complete = false if buffered.bytesize > MAX_BODY_BYTES
      end
      [buffered, [buffered], complete]
    end

    def header_value(headers, name)
      return nil unless headers.respond_to?(:each)
      headers.each do |key, value|
        return value.is_a?(Array) ? value.first.to_s : value.to_s if key.to_s.downcase == name
      end
      nil
    end

    def set_header(headers, name, value)
      headers[name] = value
      headers
    end
  end
end
