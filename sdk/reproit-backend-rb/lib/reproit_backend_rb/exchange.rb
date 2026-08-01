# Bounded dependency-exchange records: the request the app sent and the
# response the dependency returned.
#
# Ruby port of the shapes in sdk/reproit-backend-node/instrument.js. An
# exchange is what hermetic replay serves, so responses are captured verbatim
# up to a fixed inline budget; an over-budget body keeps only provable
# identity (byte count + sha256) and is marked truncated, and replay fails
# closed on it with a named reason instead of guessing.

require "digest"
require "json"

module ReproitBackendRb
  module Exchange
    # Inline body budget per exchange side, byte-identical to the Node and
    # Rust SDKs so one replay engine consumes every platform's captures.
    MAX_EXCHANGE_BODY_BYTES = 8 * 1024
    # Recorded headers are capped to keep events bounded.
    MAX_EXCHANGE_HEADERS = 32
    # Rows recorded per database result; beyond it the result is truncated.
    MAX_DB_ROWS = 64
    # Stream chunk boundaries recorded per exchange (SSE / chunked responses,
    # the LLM streaming shape). Beyond it the boundaries are marked truncated
    # and replay fails closed rather than serve a wrong stream shape.
    MAX_STREAM_CHUNKS = 128

    module_function

    # Bound one exchange body. Declared JSON is parsed so structural
    # redaction sees fields rather than text.
    def bounded_body(body, content_type)
      return {} if body.nil?
      # A BodyCollector that overflowed already reduced the body to provable
      # identity; pass it through instead of stringifying the identity hash.
      return body if body.is_a?(Hash) && body["truncated"] == true
      bytes = body.to_s.dup.force_encoding(Encoding::BINARY)
      return {} if bytes.empty?
      if bytes.bytesize > MAX_EXCHANGE_BODY_BYTES
        return {
          "bodyBytes" => bytes.bytesize,
          "bodySha256" => Digest::SHA256.hexdigest(bytes),
          "truncated" => true,
        }
      end
      text = bytes.dup.force_encoding(Encoding::UTF_8)
      if content_type.to_s.include?("application/json")
        begin
          return { "body" => JSON.parse(text) }
        rescue JSON::ParserError, EncodingError
          # Declared JSON that does not parse is recorded as text below.
        end
      end
      { "body" => text.valid_encoding? ? text : bytes.unpack1("H*") }
    end

    # Sorted BEFORE the cap. Capping arrival order records a different subset
    # whenever the caller's header order shifts, so two runs of one request
    # disagree and the capsule stops matching.
    def bounded_headers(headers)
      entries = (headers || {}).map do |name, value|
        [name.to_s.downcase, value.is_a?(Array) ? value.join(", ") : value.to_s]
      end
      entries = entries.sort_by(&:first).first(MAX_EXCHANGE_HEADERS)
      entries.empty? ? {} : { "headers" => entries.to_h }
    end

    # `request`/`response` are plain hashes of already-collected parts.
    def http(request, response)
      response_body = bounded_body(response[:body], response[:content_type])
      response_value = { "status" => response[:status] }
        .merge(bounded_headers(response[:headers]))
        .merge(response_body)
      # Stream shape (SSE / chunked): observed chunk boundaries, so the
      # whole stream is ONE logical exchange and replay can re-serve it
      # chunk for chunk. A truncated inline body already fails closed, so
      # boundaries are only kept for bodies recorded verbatim.
      stream = response[:stream]
      if stream && response_body["truncated"] != true
        response_value["stream"] = stream
      end
      {
        "protocol" => "http",
        "request" => {
          "method" => request[:method],
          "url" => request[:url],
        }.merge(bounded_headers(request[:headers]))
          .merge(bounded_body(request[:body], request[:content_type])),
        "response" => response_value,
      }
    end

    # Collect a stream's chunks up to one byte past the inline budget;
    # enough to know the true size class without holding unbounded memory.
    # The sha256 runs over EVERY byte so truncated identity stays provable.
    # Chunk boundaries are recorded as observed byte lengths, bounded by
    # MAX_STREAM_CHUNKS; boundaries past the cap are counted, never guessed.
    class BodyCollector
      def initialize
        @chunks = []
        @boundaries = []
        @bytes = 0
        @dropped_boundaries = 0
        @digest = Digest::SHA256.new
      end

      def push(chunk)
        raw = chunk.to_s.dup.force_encoding(Encoding::BINARY)
        @bytes += raw.bytesize
        @digest.update(raw)
        if @boundaries.length < MAX_STREAM_CHUNKS
          @boundaries << raw.bytesize
        else
          @dropped_boundaries += 1
        end
        @chunks << raw if @bytes <= MAX_EXCHANGE_BODY_BYTES
        nil
      end

      # The collected body: nil when empty, provable identity when over
      # budget, the raw bytes otherwise.
      def result
        return nil if @bytes.zero?
        if @bytes > MAX_EXCHANGE_BODY_BYTES
          return {
            "bodyBytes" => @bytes,
            "bodySha256" => @digest.hexdigest,
            "truncated" => true,
          }
        end
        @chunks.join
      end

      def truncated?
        @bytes > MAX_EXCHANGE_BODY_BYTES
      end

      # Chunk boundaries as observed byte lengths. Recorded when the
      # response is a stream (SSE always; anything else only when it
      # actually arrived in more than one chunk, since a single-chunk body
      # replays identically without them).
      def stream(is_event_stream)
        return nil if @boundaries.empty?
        if !is_event_stream && @boundaries.length < 2 && @dropped_boundaries.zero?
          return nil
        end
        return { "chunks" => @boundaries.dup, "truncated" => true } if @dropped_boundaries.positive?
        { "chunks" => @boundaries.dup }
      end
    end

    def db(text, values, outcome)
      request = { "text" => text.to_s }
      request["values"] = values if values.is_a?(Array) && !values.empty?
      { "protocol" => "db", "request" => request, "response" => outcome }
    end

    # Normalize a driver result into the recorded response shape. `rows` is
    # any array of row hashes; anything else records as a bare row count.
    def db_outcome(result)
      return { "rowCount" => 0 } unless result.is_a?(Hash)
      rows = result["rows"] || result[:rows]
      rows = [] unless rows.is_a?(Array)
      command = result["command"] || result[:command]
      count = result["rowCount"] || result[:rowCount] || result[:row_count]
      outcome = {
        "command" => command.nil? ? nil : command.to_s,
        "rowCount" => count.is_a?(Integer) ? count : rows.length,
        "rows" => rows.first(MAX_DB_ROWS),
      }
      outcome["truncated"] = true if rows.length > MAX_DB_ROWS
      outcome
    end

    def db_error(error)
      code = error.respond_to?(:code) ? error.code : nil
      { "error" => { "message" => error.message.to_s, "code" => code.nil? ? nil : code.to_s } }
    end

    # Effect kind for a statement: reads stay reads so state oracles keep
    # their meaning; everything else is a write.
    def statement_effect_kind(text)
      verb = text.to_s.lstrip[0, 8].to_s.upcase
      verb.start_with?("SELECT", "SHOW") ? "read" : "write"
    end
  end
end
