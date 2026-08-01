// The ADO.NET boundary for reproit-backend-dotnet.
//
// `Ado.Wrap(connection)` wraps any `DbConnection` so every command executed through it is
// recorded as a `pg` exchange on the ambient trace, exactly the wire shape the Node reference
// emits for the pg driver: request `{text, values}`, response `{command, rowCount, rows}` or
// `{error: {message, code}}`. Rows are drained once at execute time, recorded bounded at
// `Exchange.MaxDbRows` (over-cap results are marked truncated), and re-served to the
// application through an in-memory reader, so the app sees exactly the rows the driver
// returned.
//
// With `REPROIT_REPLAY` set the SAME wrapper serves the capture: `Open()` is a connect stub
// (the inner connection is never opened, so the app boots with the database down), recorded
// errors re-throw as `Ado.ReplayedException`, and a statement the capture never saw throws
// `Ado.DivergedException` (fail closed, marker on stderr via the session).
//
// Recorded row values are reduced to JSON-safe primitives (DBNull to null, integers to long,
// DateTime/Guid/byte[] to strings); replay serves those primitives back. An app comparing a
// replayed DateTime cell as a CLR DateTime observes a string instead: a NAMED limitation of
// the JSON capsule, the same one every SDK's row capture has, never a silent guess.

using System.Collections;
using System.Data;
using System.Data.Common;
using System.Diagnostics.CodeAnalysis;
using System.Globalization;

namespace ReproitBackend;

public static class Ado
{
    // Wrap one connection. Everything the app executes through the returned connection is
    // captured (or served) at the exchange boundary; the inner connection is only ever
    // touched outside replay mode.
    public static DbConnection Wrap(DbConnection inner) => new ExchangeConnection(inner);

    // A statement the capture never saw. DbException-derived so an app's existing
    // `catch (DbException)` treats it as the database failure it stands for.
    public sealed class DivergedException : DbException
    {
        internal DivergedException() : base("reproit: pg call diverged from the capture") {}
    }

    // A database error recorded at capture time, re-thrown at replay.
    public sealed class ReplayedException : DbException
    {
        private readonly string? _sqlState;

        internal ReplayedException(string message, string? sqlState) : base(message)
        {
            _sqlState = sqlState;
        }

        public override string? SqlState => _sqlState;
    }

    // The pg-style command tag for an outcome; the first SQL verb, uppercased.
    internal static string? CommandTag(string text)
    {
        var trimmed = text.TrimStart();
        var end = trimmed.IndexOf(' ');
        var verb = (end < 0 ? trimmed : trimmed[..end]).ToUpperInvariant();
        return verb.Length == 0 ? null : verb;
    }

    // Reduce one cell to the JSON-safe primitives the capsule can carry (header comment).
    internal static object? JsonSafe(object? value) => value switch
    {
        null or DBNull => null,
        bool flag => flag,
        sbyte or byte or short or ushort or int or uint or long =>
            Convert.ToInt64(value, CultureInfo.InvariantCulture),
        float or double => Convert.ToDouble(value, CultureInfo.InvariantCulture),
        decimal number => (double)number,
        string text => text,
        Guid guid => guid.ToString(),
        DateTime at => at.ToString("o", CultureInfo.InvariantCulture),
        DateTimeOffset at => at.ToString("o", CultureInfo.InvariantCulture),
        byte[] bytes => Convert.ToBase64String(bytes),
        _ => value.ToString(),
    };

    private static void Record(
        string text, List<object?>? values, Dictionary<string, object?> outcome)
    {
        var trace = Instrument.AmbientTrace();
        if (trace == null) return;
        try
        {
            trace.Effect(Exchange.DbEffectKind(text), new EffectOptions
            {
                Resource = "pg",
                Key = text[..Math.Min(text.Length, 256)],
                Exchange = Exchange.Db(text, values, outcome),
            });
            Instrument.CountCapturedExchange();
        }
        catch (Exception)
        {
            // The trace may have finished or overflowed; the host call goes on.
            Instrument.CountFailedCapture();
        }
    }

    private sealed class ExchangeConnection : DbConnection
    {
        internal readonly DbConnection Inner;
        private bool _replayOpen;

        internal ExchangeConnection(DbConnection inner)
        {
            Inner = inner;
        }

        [AllowNull]
        public override string ConnectionString
        {
            get => Inner.ConnectionString;
            set => Inner.ConnectionString = value;
        }

        public override string Database => Inner.Database;
        public override string DataSource => Inner.DataSource;
        public override string ServerVersion =>
            Instrument.Replaying() ? "reproit-replay" : Inner.ServerVersion;

        public override ConnectionState State => Instrument.Replaying()
            ? (_replayOpen ? ConnectionState.Open : ConnectionState.Closed)
            : Inner.State;

        // The connect stub: hermetic replay never dials, so the app boots with the
        // database down.
        public override void Open()
        {
            if (Instrument.Replaying())
            {
                _replayOpen = true;
                return;
            }
            Inner.Open();
        }

        public override Task OpenAsync(CancellationToken cancellationToken)
        {
            if (Instrument.Replaying())
            {
                _replayOpen = true;
                return Task.CompletedTask;
            }
            return Inner.OpenAsync(cancellationToken);
        }

        public override void Close()
        {
            if (Instrument.Replaying())
            {
                _replayOpen = false;
                return;
            }
            Inner.Close();
        }

        public override void ChangeDatabase(string databaseName)
        {
            if (Instrument.Replaying()) return;
            Inner.ChangeDatabase(databaseName);
        }

        protected override DbCommand CreateDbCommand() =>
            new ExchangeCommand(this, Inner.CreateCommand());

        protected override DbTransaction BeginDbTransaction(IsolationLevel isolationLevel) =>
            Instrument.Replaying()
                ? new ReplayTransaction(this, isolationLevel)
                : new WrappedTransaction(this, Inner.BeginTransaction(isolationLevel));

        protected override void Dispose(bool disposing)
        {
            if (disposing && !Instrument.Replaying()) Inner.Dispose();
            base.Dispose(disposing);
        }
    }

    // Replay-mode transactions are no-ops: state changes are already outcomes in the
    // capture, and there is no server to commit against.
    private sealed class ReplayTransaction : DbTransaction
    {
        private readonly DbConnection _connection;

        internal ReplayTransaction(DbConnection connection, IsolationLevel isolationLevel)
        {
            _connection = connection;
            IsolationLevel = isolationLevel;
        }

        public override IsolationLevel IsolationLevel { get; }
        protected override DbConnection DbConnection => _connection;
        public override void Commit() {}
        public override void Rollback() {}
    }

    private sealed class WrappedTransaction : DbTransaction
    {
        private readonly DbConnection _connection;
        internal readonly DbTransaction Inner;

        internal WrappedTransaction(DbConnection connection, DbTransaction inner)
        {
            _connection = connection;
            Inner = inner;
        }

        public override IsolationLevel IsolationLevel => Inner.IsolationLevel;
        protected override DbConnection DbConnection => _connection;
        public override void Commit() => Inner.Commit();
        public override void Rollback() => Inner.Rollback();

        protected override void Dispose(bool disposing)
        {
            if (disposing) Inner.Dispose();
            base.Dispose(disposing);
        }
    }

    private sealed class ExchangeCommand : DbCommand
    {
        private readonly ExchangeConnection _connection;
        private readonly DbCommand _inner;

        internal ExchangeCommand(ExchangeConnection connection, DbCommand inner)
        {
            _connection = connection;
            _inner = inner;
        }

        [AllowNull]
        public override string CommandText
        {
            get => _inner.CommandText;
            set => _inner.CommandText = value;
        }

        public override int CommandTimeout
        {
            get => _inner.CommandTimeout;
            set => _inner.CommandTimeout = value;
        }

        public override CommandType CommandType
        {
            get => _inner.CommandType;
            set => _inner.CommandType = value;
        }

        public override bool DesignTimeVisible
        {
            get => _inner.DesignTimeVisible;
            set => _inner.DesignTimeVisible = value;
        }

        public override UpdateRowSource UpdatedRowSource
        {
            get => _inner.UpdatedRowSource;
            set => _inner.UpdatedRowSource = value;
        }

        protected override DbConnection? DbConnection
        {
            get => _connection;
            set
            {
                // Rebinding to a foreign connection would bypass the boundary; refuse.
                if (!ReferenceEquals(value, _connection))
                {
                    throw new InvalidOperationException(
                        "reproit: a wrapped command stays on its wrapped connection");
                }
            }
        }

        protected override DbParameterCollection DbParameterCollection => _inner.Parameters;

        protected override DbTransaction? DbTransaction
        {
            get => null;
            set
            {
                if (value is WrappedTransaction wrapped) _inner.Transaction = wrapped.Inner;
                // A ReplayTransaction has no server side; nothing to hand the inner command.
            }
        }

        public override void Cancel() => _inner.Cancel();
        protected override DbParameter CreateDbParameter() => _inner.CreateParameter();
        public override void Prepare()
        {
            if (Instrument.Replaying()) return;
            _inner.Prepare();
        }

        private List<object?>? Values()
        {
            if (_inner.Parameters.Count == 0) return null;
            var values = new List<object?>(_inner.Parameters.Count);
            foreach (DbParameter parameter in _inner.Parameters)
            {
                values.Add(JsonSafe(parameter.Value));
            }
            return values;
        }

        private Dictionary<string, object?> Probe(string text, List<object?>? values)
        {
            var probe = new Dictionary<string, object?> { ["text"] = text };
            if (values is { Count: > 0 }) probe["values"] = values;
            return probe;
        }

        // Match one statement against the replay session. Returns the recorded outcome;
        // throws on divergence or a recorded error.
        private Dictionary<string, object?> Serve(
            Replay session, string text, List<object?>? values)
        {
            var recorded = session.Matched("pg", Probe(text, values));
            if (recorded == null) throw new DivergedException();
            var outcome = recorded.GetValueOrDefault("response") as Dictionary<string, object?>
                ?? new Dictionary<string, object?>();
            if (outcome.GetValueOrDefault("error") is Dictionary<string, object?> error)
            {
                throw new ReplayedException(
                    error.GetValueOrDefault("message") as string ?? "recorded pg error",
                    error.GetValueOrDefault("code") as string);
            }
            return outcome;
        }

        protected override DbDataReader ExecuteDbDataReader(CommandBehavior behavior)
        {
            var text = _inner.CommandText;
            var values = Values();
            var session = Instrument.Session();
            if (session != null)
            {
                var outcome = Serve(session, text, values);
                return MemoryReader.FromOutcome(outcome);
            }
            List<Dictionary<string, object?>> rows;
            DbDataReader live;
            try
            {
                live = _inner.ExecuteReader(behavior);
            }
            catch (Exception failure)
            {
                Record(text, values, Exchange.DbError(
                    failure.Message, (failure as DbException)?.SqlState));
                throw;
            }
            // Drain once, record bounded, re-serve everything: the app sees exactly the
            // rows the driver returned, and the capsule keeps the first MaxDbRows.
            using (live)
            {
                rows = new List<Dictionary<string, object?>>();
                while (live.Read())
                {
                    var row = new Dictionary<string, object?>();
                    for (var index = 0; index < live.FieldCount; index++)
                    {
                        row[live.GetName(index)] =
                            live.IsDBNull(index) ? null : JsonSafe(live.GetValue(index));
                    }
                    rows.Add(row);
                }
            }
            Record(text, values, Exchange.DbOutcome(
                CommandTag(text), rows.Count, rows.Cast<object?>().ToList()));
            return new MemoryReader(rows, rows.Count, CommandTag(text));
        }

        public override int ExecuteNonQuery()
        {
            var text = _inner.CommandText;
            var values = Values();
            var session = Instrument.Session();
            if (session != null)
            {
                var outcome = Serve(session, text, values);
                return outcome.GetValueOrDefault("rowCount") switch
                {
                    long value => (int)value,
                    int value => value,
                    double value => (int)value,
                    _ => 0,
                };
            }
            int affected;
            try
            {
                affected = _inner.ExecuteNonQuery();
            }
            catch (Exception failure)
            {
                Record(text, values, Exchange.DbError(
                    failure.Message, (failure as DbException)?.SqlState));
                throw;
            }
            Record(text, values, Exchange.DbOutcome(
                CommandTag(text), affected, Array.Empty<object?>()));
            return affected;
        }

        public override object? ExecuteScalar()
        {
            using var reader = ExecuteDbDataReader(CommandBehavior.Default);
            if (!reader.Read() || reader.FieldCount == 0) return null;
            return reader.GetValue(0);
        }

        protected override void Dispose(bool disposing)
        {
            if (disposing) _inner.Dispose();
            base.Dispose(disposing);
        }
    }

    // Rows drained at execute time (or served at replay), re-exposed through the standard
    // reader surface in driver order. Minimal on purpose: anything exotic fails loudly.
    private sealed class MemoryReader : DbDataReader
    {
        private readonly List<Dictionary<string, object?>> _rows;
        private readonly List<string> _columns;
        private readonly int _recordsAffected;
        private int _at = -1;

        internal MemoryReader(
            List<Dictionary<string, object?>> rows, int rowCount, string? command)
        {
            _rows = rows;
            _columns = rows.Count > 0 ? rows[0].Keys.ToList() : new List<string>();
            var isRead = command == "SELECT" || command == "SHOW";
            _recordsAffected = isRead ? -1 : rowCount;
        }

        internal static MemoryReader FromOutcome(Dictionary<string, object?> outcome)
        {
            var rows = new List<Dictionary<string, object?>>();
            if (outcome.GetValueOrDefault("rows") is List<object?> recorded)
            {
                foreach (var row in recorded)
                {
                    if (row is Dictionary<string, object?> map) rows.Add(map);
                }
            }
            var rowCount = outcome.GetValueOrDefault("rowCount") switch
            {
                long value => (int)value,
                int value => value,
                double value => (int)value,
                _ => rows.Count,
            };
            return new MemoryReader(
                rows, rowCount, outcome.GetValueOrDefault("command") as string);
        }

        public override int Depth => 0;
        public override int FieldCount => _columns.Count;
        public override bool HasRows => _rows.Count > 0;
        public override bool IsClosed => false;
        public override int RecordsAffected => _recordsAffected;

        public override bool Read()
        {
            if (_at + 1 >= _rows.Count) return false;
            _at += 1;
            return true;
        }

        public override bool NextResult() => false;

        private object? Cell(int ordinal) => _rows[_at][_columns[ordinal]];

        public override object GetValue(int ordinal) => Cell(ordinal) ?? DBNull.Value;
        public override bool IsDBNull(int ordinal) => Cell(ordinal) == null;
        public override string GetName(int ordinal) => _columns[ordinal];
        public override int GetOrdinal(string name) => _columns.IndexOf(name);
        public override object this[int ordinal] => GetValue(ordinal);
        public override object this[string name] => GetValue(GetOrdinal(name));

        public override Type GetFieldType(int ordinal) =>
            Cell(ordinal)?.GetType() ?? typeof(object);

        public override string GetDataTypeName(int ordinal) => GetFieldType(ordinal).Name;

        public override int GetValues(object[] values)
        {
            var count = Math.Min(values.Length, FieldCount);
            for (var index = 0; index < count; index++) values[index] = GetValue(index);
            return count;
        }

        public override bool GetBoolean(int ordinal) => (bool)GetValue(ordinal);
        public override byte GetByte(int ordinal) =>
            Convert.ToByte(GetValue(ordinal), CultureInfo.InvariantCulture);
        public override char GetChar(int ordinal) =>
            Convert.ToChar(GetValue(ordinal), CultureInfo.InvariantCulture);
        public override short GetInt16(int ordinal) =>
            Convert.ToInt16(GetValue(ordinal), CultureInfo.InvariantCulture);
        public override int GetInt32(int ordinal) =>
            Convert.ToInt32(GetValue(ordinal), CultureInfo.InvariantCulture);
        public override long GetInt64(int ordinal) =>
            Convert.ToInt64(GetValue(ordinal), CultureInfo.InvariantCulture);
        public override float GetFloat(int ordinal) =>
            Convert.ToSingle(GetValue(ordinal), CultureInfo.InvariantCulture);
        public override double GetDouble(int ordinal) =>
            Convert.ToDouble(GetValue(ordinal), CultureInfo.InvariantCulture);
        public override decimal GetDecimal(int ordinal) =>
            Convert.ToDecimal(GetValue(ordinal), CultureInfo.InvariantCulture);
        public override string GetString(int ordinal) => (string)GetValue(ordinal);
        public override Guid GetGuid(int ordinal) => Guid.Parse(GetString(ordinal));
        public override DateTime GetDateTime(int ordinal) => DateTime.Parse(
            GetString(ordinal), CultureInfo.InvariantCulture,
            DateTimeStyles.RoundtripKind);

        public override long GetBytes(
            int ordinal, long dataOffset, byte[]? buffer, int bufferOffset, int length) =>
            throw new NotSupportedException("reproit: byte-range reads are not replayable");

        public override long GetChars(
            int ordinal, long dataOffset, char[]? buffer, int bufferOffset, int length) =>
            throw new NotSupportedException("reproit: char-range reads are not replayable");

        public override IEnumerator GetEnumerator() => new DbEnumerator(this);
    }
}
