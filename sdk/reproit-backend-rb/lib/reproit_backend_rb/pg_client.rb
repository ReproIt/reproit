# pg gem (PG::Connection) wrap: the one canonical DB driver, like pg in Node.
#
# `ReproitBackendRb.wrap_pg(pg)` prepends a recorder to `pg::Connection`'s
# statement surface (`exec`, `exec_params`, `query`, `async_exec`,
# `async_exec_params`, whichever the class defines) so every statement and
# its result are recorded as a `pg` exchange on the ambient trace, exactly
# the wire shape the Node reference emits for the pg driver: request
# `{text, values}`, response `{command, rowCount, rows}` or
# `{error: {message, code}}`. Rows are the driver's own hashes, bounded at
# MAX_DB_ROWS with a truncation marker.
#
# With REPROIT_REPLAY set, `pg.connect` and `pg::Connection.new/connect/open`
# return an in-process stub served from the recorded exchanges: no server is
# dialed (the app boots with the database down), a recorded error re-raises,
# and a statement the capture never saw raises DivergenceError (fail closed,
# marker on stderr via the session).
#
# Accepts the real gem module or any module-shaped object exposing the same
# Connection surface. Only string statements with positional parameters are
# recorded; exotic forms (COPY, prepared statements by name, pipelines) pass
# through unrecorded rather than half-recorded, matching Node's wrapPg
# decision: a NAMED capability gap, never a silent downgrade.

require_relative "exchange"
require_relative "instrument"

module ReproitBackendRb
  module PgClient
    STATEMENT_METHODS = %i[exec exec_params query async_exec async_exec_params].freeze

    module_function

    # Patch the module (or module-shaped object). Idempotent.
    def wrap(pg)
      return pg if pg.nil?
      connection = pg.respond_to?(:const_defined?) && pg.const_defined?(:Connection, false) &&
                   pg.const_get(:Connection, false)
      return pg unless connection.is_a?(Class)
      return pg if connection.instance_variable_get(:@reproit_wrapped)
      connection.instance_variable_set(:@reproit_wrapped, true)
      wrap_statements(connection)
      wrap_constructors(connection)
      if pg.respond_to?(:connect)
        pg.singleton_class.prepend(module_connect_hook)
      end
      pg
    end

    def probe(text, values)
      request = { "text" => text.to_s }
      request["values"] = values if values.is_a?(Array) && !values.empty?
      request
    end

    # Normalize a PG::Result-shaped object into the recorded response shape.
    def outcome(result)
      rows = result.respond_to?(:to_a) ? result.to_a : []
      rows = [] unless rows.is_a?(Array)
      status = result.respond_to?(:cmd_status) ? result.cmd_status.to_s : ""
      count = result.respond_to?(:cmd_tuples) ? result.cmd_tuples : nil
      Exchange.db_outcome(
        "command" => status.empty? ? nil : status.split(" ").first,
        "rowCount" => count.is_a?(Integer) && count >= 0 ? count : rows.length,
        "rows" => rows
      )
    end

    def record(text, values, response)
      Instrument.record(
        Exchange.statement_effect_kind(text), "pg", text.to_s[0, 256],
        { "protocol" => "pg", "request" => probe(text, values), "response" => response }
      )
    end

    # Match one statement against the replay session. Returns the outcome
    # hash; raises on divergence or a recorded error.
    def serve(session, text, values)
      recorded = session.match("pg", probe(text, values))
      if recorded.nil?
        raise Instrument::DivergenceError, "reproit: pg call diverged from the capture"
      end
      response = recorded["response"] || {}
      if response["error"]
        error = Instrument::RecordedError.new(response["error"]["message"].to_s)
        error.define_singleton_method(:code) { response["error"]["code"] }
        raise error
      end
      response
    end

    def wrap_statements(connection)
      recorder = Module.new do
        PgClient::STATEMENT_METHODS.each do |name|
          next unless connection.method_defined?(name)
          define_method(name) do |*args, &block|
            text = args[0]
            values = args[1]
            unless text.is_a?(String) && Instrument.current_trace
              next super(*args, &block)
            end
            begin
              result = super(*args, &block)
            rescue StandardError => error
              PgClient.record(text, values, Exchange.db_error(error))
              raise
            end
            PgClient.record(text, values, PgClient.outcome(result))
            result
          end
        end
      end
      connection.prepend(recorder)
    end

    # Hermetic replay never dials: every constructor returns the stub.
    def wrap_constructors(connection)
      hook = Module.new do
        %i[new connect open setdb setdblogin].each do |name|
          define_method(name) do |*args, &block|
            session = Instrument.session
            next super(*args, &block) if session.nil?
            stub = ReplayConnection.new(session)
            block ? block.call(stub) : stub
          end
        end
      end
      connection.singleton_class.prepend(hook)
    end

    def module_connect_hook
      Module.new do
        define_method(:connect) do |*args, &block|
          session = Instrument.session
          next super(*args, &block) if session.nil?
          stub = ReplayConnection.new(session)
          block ? block.call(stub) : stub
        end
      end
    end

    # Result stub served entirely from the capture. Minimal on purpose: the
    # read surface the fixture and common apps use; anything else fails
    # loudly with NoMethodError.
    class ReplayResult
      include Enumerable

      def initialize(response)
        @rows = response["rows"].is_a?(Array) ? response["rows"] : []
        @response = response
      end

      def to_a
        @rows.dup
      end

      def each(&block)
        @rows.each(&block)
      end

      def first
        @rows.first
      end

      def [](index)
        @rows[index]
      end

      def ntuples
        @rows.length
      end

      def cmd_tuples
        count = @response["rowCount"]
        count.is_a?(Integer) ? count : @rows.length
      end

      def cmd_status
        @response["command"].to_s
      end

      def clear
        nil
      end
    end

    # Connection stub for hermetic replay: no server is ever dialed.
    class ReplayConnection
      def initialize(session)
        @session = session
        @finished = false
      end

      PgClient::STATEMENT_METHODS.each do |name|
        define_method(name) do |text, values = nil, &block|
          result = ReplayResult.new(PgClient.serve(@session, text, values))
          block ? block.call(result) : result
        end
      end

      def transaction
        yield self
      end

      def close
        @finished = true
        nil
      end
      alias finish close

      def finished?
        @finished
      end
    end
  end

  # Public entry point, like the Node reference's wrapPg and the Python
  # port's wrap_psycopg.
  def self.wrap_pg(pg)
    PgClient.wrap(pg)
  end
end
