// Money-test fixture for .NET capsule parity: an ASP.NET Core minimal API whose /quote
// operation 500s because an upstream pricing service returns {"prices": null} and the
// handler indexes into it. The upstream call goes through `Instrument.Handler()` (the
// HttpMessageHandler boundary) and the database call through a fake ADO.NET driver wrapped
// by `Ado.Wrap` (the same fake-driver idiom the Python fixture uses: a driver that MUST
// never be reached during hermetic replay).
//
// MODE=capture boots the upstream plus the app, fires the failing request, and writes a
// version-2 reproit-backend-capture (exchanges plus envelope) to CAPTURE_OUT. Default
// (server) mode boots ONLY the app on $PORT; with REPROIT_REPLAY set the SDK serves the
// recorded exchanges in process, so neither the upstream nor the database exists. FIXED=1
// applies the fix.

using System.Data;
using System.Data.Common;
using System.Text;
using ReproitBackend;

const int UpstreamPort = 19976;
const int CapturePort = 19975;
var upstreamUrl = $"http://127.0.0.1:{UpstreamPort}/prices?tier=gold";
var capturing = Environment.GetEnvironmentVariable("MODE") == "capture";
var fixedBuild = Environment.GetEnvironmentVariable("FIXED") == "1";
Instrument.Init();

// The upstream dependency exists only while capturing; replay serves it from the recording.
WebApplication? upstream = null;
if (capturing)
{
    var upstreamBuilder = WebApplication.CreateBuilder();
    upstreamBuilder.WebHost.UseUrls($"http://127.0.0.1:{UpstreamPort}");
    upstreamBuilder.Logging.ClearProviders();
    upstream = upstreamBuilder.Build();
    upstream.MapGet("/prices", () => Results.Json(new Dictionary<string, object?>
    {
        ["prices"] = null,
    }));
    await upstream.StartAsync();
}

var traces = new List<BackendTrace>();
var client = new HttpClient(Instrument.Handler());
var database = Ado.Wrap(new FakeDbConnection(capturing));

var builder = WebApplication.CreateBuilder(args);
var port = capturing
    ? CapturePort
    : int.Parse(Environment.GetEnvironmentVariable("PORT") ?? CapturePort.ToString());
builder.WebHost.UseUrls($"http://127.0.0.1:{port}");
builder.Logging.ClearProviders();
var app = builder.Build();

// The trace boundary, hand-rolled here so the fixture needs no cloud endpoint: it scopes
// the handler exactly as the shipped ASP.NET Core middleware does.
app.Use(async (context, next) =>
{
    var trace = BackendTrace.Begin(
        new TraceContext
        {
            TraceId = "cap-dotnet-money-1",
            Build = "dotnet-money-fixture",
            CaptureEnvelope = true,
        },
        context.Request.Method + " " + context.Request.Path,
        new BeginOptions
        {
            Input = Reproit.HttpInput(
                null,
                null,
                context.Request.Query.ToDictionary(
                    entry => entry.Key, entry => (object?)entry.Value.ToString()),
                null),
        });
    traces.Add(trace);
    await Instrument.ScopeAsync(trace, async () => await next(context));
    if (!trace.Finished)
    {
        trace.Finish(null, context.Response.StatusCode, context.Response.StatusCode < 500, true);
    }
});

app.MapGet("/quote", async (HttpContext context) =>
{
    try
    {
        var symbol = context.Request.Query["symbol"].ToString();
        // The database leg: through the wrapped ADO.NET connection. During replay the
        // connect stub serves the recorded rows and the fake driver is never reached.
        database.Open();
        try
        {
            using var command = database.CreateCommand();
            command.CommandText = "SELECT id, symbol FROM issuers WHERE symbol = $1";
            var parameter = command.CreateParameter();
            parameter.Value = symbol;
            command.Parameters.Add(parameter);
            using var reader = command.ExecuteReader();
            if (!reader.Read())
            {
                return Results.Json(
                    new Dictionary<string, object?> { ["error"] = "unknown symbol" },
                    statusCode: 404);
            }
        }
        finally
        {
            database.Close();
        }
        var response = await client.GetAsync(upstreamUrl);
        var body = Json.Parse(await response.Content.ReadAsStringAsync())
            as Dictionary<string, object?> ?? new Dictionary<string, object?>();
        var prices = body.GetValueOrDefault("prices");
        if (fixedBuild && prices is not List<object?>)
        {
            return Results.Json(new Dictionary<string, object?>
            {
                ["first"] = null,
                ["note"] = "no prices",
            });
        }
        var first = ((List<object?>)prices!)[0];
        return Results.Json(new Dictionary<string, object?> { ["first"] = first });
    }
    catch (Exception)
    {
        return Results.Json(
            new Dictionary<string, object?> { ["error"] = "internal" }, statusCode: 500);
    }
});

await app.StartAsync();

if (!capturing)
{
    await app.WaitForShutdownAsync();
    return;
}

using var driver = new HttpClient();
var failing = await driver.GetAsync($"http://127.0.0.1:{port}/quote?symbol=ACME");
Console.WriteLine("capture fixture status " + (int)failing.StatusCode);
var recorded = traces[^1];
if (!recorded.Finished) recorded.Finish(null, (int)failing.StatusCode, false, true);
WriteCapture(recorded);
await app.StopAsync();
if (upstream != null) await upstream.StopAsync();

static void WriteCapture(BackendTrace trace)
{
    var first = trace.Events()[0];
    var payload = new Dictionary<string, object?>
    {
        ["format"] = Capture.CaptureFormat,
        ["version"] = 2L,
        ["operation"] = first.GetValueOrDefault("operation"),
        ["oracle"] = Capture.ServerErrorOracle,
        ["envelope"] = Capture.DeterminismEnvelope(first.GetValueOrDefault("at") as long?),
        ["events"] = trace.Events().Cast<object?>().ToList(),
    };
    // No BOM: a capture is consumed as plain JSON by the CLI and every other SDK, and
    // Encoding.UTF8 would prepend one.
    File.WriteAllText(
        Environment.GetEnvironmentVariable("CAPTURE_OUT")!,
        Json.Canonical(payload),
        new UTF8Encoding(false));
}

// A psycopg-shaped stand-in at the ADO.NET surface: canned rows while capturing, and an
// assertion that hermetic replay never dials it.
internal sealed class FakeDbConnection : DbConnection
{
    private readonly bool _capturing;
    private ConnectionState _state = ConnectionState.Closed;

    internal FakeDbConnection(bool capturing)
    {
        _capturing = capturing;
    }

    [System.Diagnostics.CodeAnalysis.AllowNull]
    public override string ConnectionString { get; set; } = "postgresql://db.internal/quotes";
    public override string Database => "quotes";
    public override string DataSource => "db.internal";
    public override string ServerVersion => "0";
    public override ConnectionState State => _state;

    public override void Open()
    {
        if (!_capturing)
        {
            throw new InvalidOperationException("live database dialed during hermetic replay");
        }
        _state = ConnectionState.Open;
    }

    public override void Close() => _state = ConnectionState.Closed;
    public override void ChangeDatabase(string databaseName) {}
    protected override DbCommand CreateDbCommand() => new FakeDbCommand(_capturing);
    protected override DbTransaction BeginDbTransaction(IsolationLevel isolationLevel) =>
        throw new NotSupportedException();
}

internal sealed class FakeDbCommand : DbCommand
{
    private readonly bool _capturing;
    private readonly FakeParameterCollection _parameters = new();

    internal FakeDbCommand(bool capturing)
    {
        _capturing = capturing;
    }

    [System.Diagnostics.CodeAnalysis.AllowNull]
    public override string CommandText { get; set; } = string.Empty;
    public override int CommandTimeout { get; set; }
    public override CommandType CommandType { get; set; }
    public override bool DesignTimeVisible { get; set; }
    public override UpdateRowSource UpdatedRowSource { get; set; }
    protected override DbConnection? DbConnection { get; set; }
    protected override DbParameterCollection DbParameterCollection => _parameters;
    protected override DbTransaction? DbTransaction { get; set; }
    public override void Cancel() {}
    public override void Prepare() {}
    protected override DbParameter CreateDbParameter() => new FakeParameter();
    public override int ExecuteNonQuery() => 0;
    public override object? ExecuteScalar() => null;

    protected override DbDataReader ExecuteDbDataReader(CommandBehavior behavior)
    {
        if (!_capturing)
        {
            throw new InvalidOperationException("live database reached during hermetic replay");
        }
        return new FakeReader(new[]
        {
            new Dictionary<string, object?> { ["id"] = 7L, ["symbol"] = "ACME" },
        });
    }
}

internal sealed class FakeParameter : DbParameter
{
    public override DbType DbType { get; set; }
    public override ParameterDirection Direction { get; set; }
    public override bool IsNullable { get; set; }
    [System.Diagnostics.CodeAnalysis.AllowNull]
    public override string ParameterName { get; set; } = string.Empty;
    [System.Diagnostics.CodeAnalysis.AllowNull]
    public override string SourceColumn { get; set; } = string.Empty;
    public override bool SourceColumnNullMapping { get; set; }
    public override object? Value { get; set; }
    public override int Size { get; set; }
    public override void ResetDbType() {}
}

internal sealed class FakeParameterCollection : DbParameterCollection
{
    private readonly List<object> _items = new();
    public override int Count => _items.Count;
    public override object SyncRoot => _items;
    public override int Add(object value)
    {
        _items.Add(value);
        return _items.Count - 1;
    }
    public override void AddRange(Array values)
    {
        foreach (var value in values) _items.Add(value);
    }
    public override void Clear() => _items.Clear();
    public override bool Contains(object value) => _items.Contains(value);
    public override bool Contains(string value) => false;
    public override void CopyTo(Array array, int index) {}
    public override System.Collections.IEnumerator GetEnumerator() => _items.GetEnumerator();
    public override int IndexOf(object value) => _items.IndexOf(value);
    public override int IndexOf(string parameterName) => -1;
    public override void Insert(int index, object value) => _items.Insert(index, value);
    public override void Remove(object value) => _items.Remove(value);
    public override void RemoveAt(int index) => _items.RemoveAt(index);
    public override void RemoveAt(string parameterName) {}
    protected override DbParameter GetParameter(int index) => (DbParameter)_items[index];
    protected override DbParameter GetParameter(string parameterName) =>
        throw new NotSupportedException();
    protected override void SetParameter(int index, DbParameter value) =>
        _items[index] = value;
    protected override void SetParameter(string parameterName, DbParameter value) =>
        throw new NotSupportedException();
}

internal sealed class FakeReader : DbDataReader
{
    private readonly Dictionary<string, object?>[] _rows;
    private readonly List<string> _columns;
    private int _at = -1;

    internal FakeReader(Dictionary<string, object?>[] rows)
    {
        _rows = rows;
        _columns = rows.Length > 0 ? rows[0].Keys.ToList() : new List<string>();
    }

    public override int Depth => 0;
    public override int FieldCount => _columns.Count;
    public override bool HasRows => _rows.Length > 0;
    public override bool IsClosed => false;
    public override int RecordsAffected => -1;
    public override bool Read() => ++_at < _rows.Length;
    public override bool NextResult() => false;
    public override object GetValue(int ordinal) =>
        _rows[_at][_columns[ordinal]] ?? DBNull.Value;
    public override bool IsDBNull(int ordinal) => _rows[_at][_columns[ordinal]] == null;
    public override string GetName(int ordinal) => _columns[ordinal];
    public override int GetOrdinal(string name) => _columns.IndexOf(name);
    public override object this[int ordinal] => GetValue(ordinal);
    public override object this[string name] => GetValue(GetOrdinal(name));
    public override Type GetFieldType(int ordinal) =>
        _rows[_at][_columns[ordinal]]?.GetType() ?? typeof(object);
    public override string GetDataTypeName(int ordinal) => GetFieldType(ordinal).Name;
    public override int GetValues(object[] values)
    {
        var count = Math.Min(values.Length, FieldCount);
        for (var index = 0; index < count; index++) values[index] = GetValue(index);
        return count;
    }
    public override bool GetBoolean(int ordinal) => (bool)GetValue(ordinal);
    public override byte GetByte(int ordinal) => (byte)GetValue(ordinal);
    public override char GetChar(int ordinal) => (char)GetValue(ordinal);
    public override short GetInt16(int ordinal) => (short)GetValue(ordinal);
    public override int GetInt32(int ordinal) => (int)GetValue(ordinal);
    public override long GetInt64(int ordinal) => (long)GetValue(ordinal);
    public override float GetFloat(int ordinal) => (float)GetValue(ordinal);
    public override double GetDouble(int ordinal) => (double)GetValue(ordinal);
    public override decimal GetDecimal(int ordinal) => (decimal)GetValue(ordinal);
    public override string GetString(int ordinal) => (string)GetValue(ordinal);
    public override Guid GetGuid(int ordinal) => (Guid)GetValue(ordinal);
    public override DateTime GetDateTime(int ordinal) => (DateTime)GetValue(ordinal);
    public override long GetBytes(
        int ordinal, long dataOffset, byte[]? buffer, int bufferOffset, int length) =>
        throw new NotSupportedException();
    public override long GetChars(
        int ordinal, long dataOffset, char[]? buffer, int bufferOffset, int length) =>
        throw new NotSupportedException();
    public override System.Collections.IEnumerator GetEnumerator() => new DbEnumerator(this);
}
