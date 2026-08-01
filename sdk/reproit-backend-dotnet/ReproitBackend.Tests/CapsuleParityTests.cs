// Capsule parity behaviors ported from the Node reference and the Python template:
// per-operation ordinal matching, the bodyDelta divergence report (message index for
// chat-shaped bodies, byte offset fallback, ABSENT distinct from null), stream chunk
// recording through the TEE and chunk-for-chunk replay, the ADO.NET wrap (capture, replay
// through the connect stub, fail-closed divergence), and the envelope pins the SDK exposes
// (seeded RandomSource, pinned TimeProvider).

using System.Data;
using System.Data.Common;
using System.Net;
using System.Text;
using ReproitBackend;
using Xunit;

namespace ReproitBackend.Tests;

// One collection with InstrumentTests: both mutate the process-global replay session and
// redirect Console.Error, which parallel classes would race on.
[Collection("replay-session")]
public class CapsuleParityTests : IDisposable
{
    public void Dispose() => Instrument.ResetSessionForTest(null);

    private static readonly TraceContext Context = new()
    {
        TraceId = "cap-parity-1",
        CaptureEnvelope = true,
    };

    private static BackendTrace Trace() =>
        BackendTrace.Begin(Context, "GET /quote", new BeginOptions());

    private static Replay LoadCapsule(Dictionary<string, object?> capsule)
    {
        var path = Path.Combine(Path.GetTempPath(), "reproit-dotnet-capsule-" +
            Guid.NewGuid().ToString("n") + ".json");
        File.WriteAllText(path, Json.Canonical(capsule), new UTF8Encoding(false));
        var session = Replay.Load(path);
        File.Delete(path);
        Assert.NotNull(session);
        return session!;
    }

    private static Dictionary<string, object?> Capsule(params object?[] exchanges) => new()
    {
        ["format"] = "reproit-backend-capture",
        ["version"] = 2L,
        ["operation"] = "GET /quote",
        ["oracle"] = "backend-server-error",
        ["envelope"] = new Dictionary<string, object?>
        {
            ["observedAtMs"] = 1753747200000L,
            ["tz"] = "Europe/Berlin",
            ["replaySeed"] = "00ff00ff00ff00ff",
        },
        ["events"] = exchanges.Select((exchange, index) => (object?)
            new Dictionary<string, object?>
            {
                ["kind"] = "effect",
                ["sequence"] = (long)(index + 1),
                ["exchange"] = exchange,
            }).ToList(),
    };

    private static Dictionary<string, object?> HttpExchange(
        string method, string url, object? responseBody, object? requestBody = null) => new()
    {
        ["protocol"] = "http",
        ["request"] = requestBody == null
            ? new Dictionary<string, object?> { ["method"] = method, ["url"] = url }
            : new Dictionary<string, object?>
            {
                ["method"] = method,
                ["url"] = url,
                ["body"] = requestBody,
            },
        ["response"] = new Dictionary<string, object?>
        {
            ["status"] = 200L,
            ["headers"] = new Dictionary<string, object?>
            {
                ["content-type"] = "application/json",
            },
            ["body"] = responseBody,
        },
    };

    private static Dictionary<string, object?> PgExchange(string text, object? rows) => new()
    {
        ["protocol"] = "pg",
        ["request"] = new Dictionary<string, object?> { ["text"] = text },
        ["response"] = new Dictionary<string, object?>
        {
            ["command"] = "SELECT",
            ["rowCount"] = 1L,
            ["rows"] = rows,
        },
    };

    // --- per-operation ordinals ---------------------------------------------------------

    // Interleaved operations must not block each other: within one operation exchanges are
    // consumed in recorded order, but OTHER operations' exchanges may interleave, the
    // pooled-client / tool-call-loop shape.
    [Fact]
    public void InterleavedOperationsMatchByPerOperationOrdinals()
    {
        var session = LoadCapsule(Capsule(
            HttpExchange("GET", "http://a.internal/one", "first-one"),
            HttpExchange("GET", "http://b.internal/two", "first-two"),
            HttpExchange("GET", "http://a.internal/one", "second-one")));
        var probeTwo = new Dictionary<string, object?>
        {
            ["method"] = "GET",
            ["url"] = "http://b.internal/two",
        };
        // Consuming operation "two" FIRST must not consume or skip operation "one".
        Assert.Equal("first-two", ServedBody(session, probeTwo));
        Assert.Equal("first-one", ServedBody(session, ProbeOne()));
        Assert.Equal("second-one", ServedBody(session, ProbeOne()));

        static Dictionary<string, object?> ProbeOne() => new()
        {
            ["method"] = "GET",
            ["url"] = "http://a.internal/one",
        };

        static string ServedBody(Replay session, Dictionary<string, object?> probe) =>
            session.ServeHttp(probe).BodyText;
    }

    [Fact]
    public void WithinOneOperationTheNextUnconsumedExchangeIsTheOnlyCandidate()
    {
        var session = LoadCapsule(Capsule(
            HttpExchange("POST", "http://svc/c", "one", requestBody: new Dictionary<string, object?>
            {
                ["n"] = 1L,
            }),
            HttpExchange("POST", "http://svc/c", "two", requestBody: new Dictionary<string, object?>
            {
                ["n"] = 2L,
            })));
        // A probe matching the SECOND exchange of the operation must diverge: skipping the
        // first silently would be a fuzzy match.
        var held = new StringWriter();
        var original = Console.Error;
        Console.SetError(held);
        try
        {
            var served = session.ServeHttp(new Dictionary<string, object?>
            {
                ["method"] = "POST",
                ["url"] = "http://svc/c",
                ["body"] = new Dictionary<string, object?> { ["n"] = 2L },
            });
            Assert.Equal(599, served.Status);
        }
        finally
        {
            Console.SetError(original);
        }
        Assert.StartsWith(Replay.DivergenceMarker, held.ToString());
    }

    // --- bodyDelta ----------------------------------------------------------------------

    [Fact]
    public void PromptDriftNamesTheFirstDifferingMessageIndex()
    {
        List<object?> Messages(string third) => new()
        {
            new Dictionary<string, object?> { ["role"] = "user", ["content"] = "hello" },
            new Dictionary<string, object?> { ["role"] = "assistant", ["content"] = "hi" },
            new Dictionary<string, object?> { ["role"] = "user", ["content"] = third },
        };
        var delta = Replay.BodyDelta(
            new Dictionary<string, object?> { ["messages"] = Messages("weather?") },
            new Dictionary<string, object?> { ["messages"] = Messages("DIFFERENT") });
        Assert.NotNull(delta);
        Assert.Equal("message", delta!["kind"]);
        Assert.Equal(2L, delta["firstDifferingMessage"]);
        Assert.Equal(3L, delta["recordedMessages"]);
        Assert.Equal(3L, delta["liveMessages"]);
    }

    [Fact]
    public void ALongerConversationNamesTheFirstUnsharedMessage()
    {
        var recorded = new Dictionary<string, object?>
        {
            ["messages"] = new List<object?>
            {
                new Dictionary<string, object?> { ["role"] = "user", ["content"] = "hello" },
            },
        };
        var live = new Dictionary<string, object?>
        {
            ["messages"] = new List<object?>
            {
                new Dictionary<string, object?> { ["role"] = "user", ["content"] = "hello" },
                new Dictionary<string, object?> { ["role"] = "user", ["content"] = "more" },
            },
        };
        var delta = Replay.BodyDelta(recorded, live);
        Assert.NotNull(delta);
        Assert.Equal("message", delta!["kind"]);
        Assert.Equal(1L, delta["firstDifferingMessage"]);
    }

    [Fact]
    public void UnknownBodyShapesFallBackToTheByteOffset()
    {
        var delta = Replay.BodyDelta("abcdef", "abcXef");
        Assert.NotNull(delta);
        Assert.Equal("byte", delta!["kind"]);
        Assert.Equal(3L, delta["offset"]);
    }

    // ABSENT (no body key) and an explicit null are different claims: an absent side means
    // there is nothing to report, a null recorded body matches anything.
    [Fact]
    public void AbsentBodiesReportNoDeltaAndAreDistinctFromNull()
    {
        Assert.Null(Replay.BodyDelta(Replay.Absent, "anything"));
        Assert.Null(Replay.BodyDelta("anything", Replay.Absent));
        // A recorded null matches any live value (the placeholder rule), so no delta.
        Assert.Null(Replay.BodyDelta(null, "anything"));
        // A live null against recorded content IS a difference.
        Assert.NotNull(Replay.BodyDelta("recorded", null));
    }

    [Fact]
    public void TheDivergenceReportCarriesTheBodyDelta()
    {
        var session = LoadCapsule(Capsule(HttpExchange(
            "POST", "http://llm.internal/v1/chat", "ok",
            requestBody: new Dictionary<string, object?>
            {
                ["messages"] = new List<object?>
                {
                    new Dictionary<string, object?>
                    {
                        ["role"] = "user",
                        ["content"] = "hello",
                    },
                },
            })));
        var held = new StringWriter();
        var original = Console.Error;
        Console.SetError(held);
        try
        {
            session.ServeHttp(new Dictionary<string, object?>
            {
                ["method"] = "POST",
                ["url"] = "http://llm.internal/v1/chat",
                ["body"] = new Dictionary<string, object?>
                {
                    ["messages"] = new List<object?>
                    {
                        new Dictionary<string, object?>
                        {
                            ["role"] = "user",
                            ["content"] = "DIFFERENT",
                        },
                    },
                },
            });
        }
        finally
        {
            Console.SetError(original);
        }
        var marker = held.ToString().Split('\n')
            .First(line => line.StartsWith(Replay.DivergenceMarker));
        var report = (Dictionary<string, object?>)
            Json.Parse(marker[Replay.DivergenceMarker.Length..])!;
        var delta = (Dictionary<string, object?>)report["bodyDelta"]!;
        Assert.Equal("message", delta["kind"]);
        Assert.Equal(0L, delta["firstDifferingMessage"]);
    }

    // --- streams ------------------------------------------------------------------------

    private sealed class StreamingHandler : HttpMessageHandler
    {
        private readonly string[] _chunks;
        private readonly string _contentType;

        internal StreamingHandler(string[] chunks, string contentType)
        {
            _chunks = chunks;
            _contentType = contentType;
        }

        protected override Task<HttpResponseMessage> SendAsync(
            HttpRequestMessage request, CancellationToken cancellationToken)
        {
            // A push-style content whose stream yields one chunk per read, the SSE shape.
            var payload = _chunks.Select(chunk => Encoding.UTF8.GetBytes(chunk)).ToList();
            var content = new StreamContent(new OneChunkPerReadStream(payload));
            content.Headers.TryAddWithoutValidation("Content-Type", _contentType);
            // No Content-Length: the transfer is chunked, exactly the streamed case.
            return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = content,
            });
        }
    }

    private sealed class OneChunkPerReadStream : Stream
    {
        private readonly List<byte[]> _chunks;
        private int _chunk;
        private int _offset;

        internal OneChunkPerReadStream(List<byte[]> chunks)
        {
            _chunks = chunks;
        }

        public override bool CanRead => true;
        public override bool CanSeek => false;
        public override bool CanWrite => false;
        public override long Length => throw new NotSupportedException();
        public override long Position
        {
            get => throw new NotSupportedException();
            set => throw new NotSupportedException();
        }

        public override int Read(byte[] buffer, int offset, int count)
        {
            while (_chunk < _chunks.Count && _offset >= _chunks[_chunk].Length)
            {
                _chunk += 1;
                _offset = 0;
            }
            if (_chunk >= _chunks.Count) return 0;
            var current = _chunks[_chunk];
            var take = Math.Min(count, current.Length - _offset);
            Array.Copy(current, _offset, buffer, offset, take);
            _offset += take;
            return take;
        }

        public override void Flush() {}
        public override long Seek(long offset, SeekOrigin origin) =>
            throw new NotSupportedException();
        public override void SetLength(long value) => throw new NotSupportedException();
        public override void Write(byte[] buffer, int offset, int count) =>
            throw new NotSupportedException();
    }

    [Fact]
    public async Task StreamedResponsesRecordChunkBoundariesAsTheAppConsumes()
    {
        var chunks = new[] { "data: a\n\n", "data: b\n\n", "data: c\n\n" };
        var client = new HttpClient(Instrument.Handler(
            new StreamingHandler(chunks, "text/event-stream")));
        var trace = Trace();
        await Instrument.ScopeAsync(trace, async () =>
        {
            var response = await client.GetAsync(
                "http://llm.internal/stream", HttpCompletionOption.ResponseHeadersRead);
            // The app consumes the stream to the end; recording rides the same reads.
            await response.Content.ReadAsStringAsync();
        });
        var exchange = trace.Events()
            .Select(evt => evt.GetValueOrDefault("exchange") as Dictionary<string, object?>)
            .First(found => found != null)!;
        var response = (Dictionary<string, object?>)exchange["response"]!;
        Assert.Equal("data: a\n\ndata: b\n\ndata: c\n\n", response["body"]);
        var stream = (Dictionary<string, object?>)response["stream"]!;
        var boundaries = (List<object?>)stream["chunks"]!;
        Assert.Equal(new object?[] { 9L, 9L, 9L }, boundaries);
        Assert.False(stream.ContainsKey("truncated"));
    }

    [Fact]
    public async Task AnAbandonedStreamRecordsNothing()
    {
        var client = new HttpClient(Instrument.Handler(new StreamingHandler(
            new[] { "data: a\n\n", "data: b\n\n" }, "text/event-stream")));
        var trace = Trace();
        await Instrument.ScopeAsync(trace, async () =>
        {
            var response = await client.GetAsync(
                "http://llm.internal/stream", HttpCompletionOption.ResponseHeadersRead);
            // The app walks away without reading the body.
            response.Dispose();
            await Task.CompletedTask;
        });
        Assert.DoesNotContain(trace.Events(), evt => evt.ContainsKey("exchange"));
    }

    [Fact]
    public async Task ReplayServesTheRecordedStreamChunkForChunk()
    {
        var exchange = HttpExchange("GET", "http://llm.internal/stream",
            "data: a\n\ndata: b\n\ndata: c\n\n");
        var response = (Dictionary<string, object?>)exchange["response"]!;
        ((Dictionary<string, object?>)response["headers"]!)["content-type"] =
            "text/event-stream";
        response["stream"] = new Dictionary<string, object?>
        {
            ["chunks"] = new List<object?> { 9L, 9L, 9L },
        };
        Instrument.ResetSessionForTest(LoadCapsule(Capsule(exchange)));
        var client = new HttpClient(Instrument.Handler(new ThrowingHandler()));
        var served = await client.GetAsync(
            "http://llm.internal/stream", HttpCompletionOption.ResponseHeadersRead);
        var body = await served.Content.ReadAsStreamAsync();
        var buffer = new byte[64];
        var observed = new List<string>();
        while (true)
        {
            var read = body.Read(buffer, 0, buffer.Length);
            if (read == 0) break;
            observed.Add(Encoding.UTF8.GetString(buffer, 0, read));
        }
        // Each recorded chunk is observed as its own read: the stream SHAPE replays.
        Assert.Equal(new[] { "data: a\n\n", "data: b\n\n", "data: c\n\n" }, observed);
    }

    [Fact]
    public void TruncatedStreamBoundariesFailClosed()
    {
        var exchange = HttpExchange("GET", "http://llm.internal/stream", "abc");
        var response = (Dictionary<string, object?>)exchange["response"]!;
        response["stream"] = new Dictionary<string, object?>
        {
            ["chunks"] = new List<object?> { 1L, 1L, 1L },
            ["truncated"] = true,
        };
        var session = LoadCapsule(Capsule(exchange));
        var original = Console.Error;
        Console.SetError(new StringWriter());
        Replay.Served served;
        try
        {
            served = session.ServeHttp(new Dictionary<string, object?>
            {
                ["method"] = "GET",
                ["url"] = "http://llm.internal/stream",
            });
        }
        finally
        {
            Console.SetError(original);
        }
        Assert.Equal(599, served.Status);
        Assert.Contains("truncated-stream-boundaries", served.BodyText);
    }

    private sealed class ThrowingHandler : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(
            HttpRequestMessage request, CancellationToken cancellationToken) =>
            throw new InvalidOperationException("replay must never open a socket");
    }

    // --- ADO.NET wrap -------------------------------------------------------------------

    [Fact]
    public async Task AdoCommandsRecordThePgWireShape()
    {
        var connection = Ado.Wrap(new FakeConnection(rows: new[]
        {
            new Dictionary<string, object?> { ["id"] = 7L, ["symbol"] = "ACME" },
        }));
        var trace = Trace();
        await Instrument.ScopeAsync(trace, () =>
        {
            connection.Open();
            using var command = connection.CreateCommand();
            command.CommandText = "SELECT id, symbol FROM issuers WHERE symbol = $1";
            var parameter = command.CreateParameter();
            parameter.Value = "ACME";
            command.Parameters.Add(parameter);
            using var reader = command.ExecuteReader();
            Assert.True(reader.Read());
            Assert.Equal(7L, reader.GetInt64(reader.GetOrdinal("id")));
            Assert.Equal("ACME", reader.GetString(reader.GetOrdinal("symbol")));
            Assert.False(reader.Read());
            return Task.CompletedTask;
        });
        var recorded = trace.Events().First(evt => evt.ContainsKey("exchange"));
        Assert.Equal("read", recorded["effect"]);
        var exchange = (Dictionary<string, object?>)recorded["exchange"]!;
        Assert.Equal("pg", exchange["protocol"]);
        var request = (Dictionary<string, object?>)exchange["request"]!;
        Assert.Equal("SELECT id, symbol FROM issuers WHERE symbol = $1", request["text"]);
        Assert.Equal(new object?[] { "ACME" }, (List<object?>)request["values"]!);
        var response = (Dictionary<string, object?>)exchange["response"]!;
        Assert.Equal("SELECT", response["command"]);
        Assert.Equal(1L, response["rowCount"]);
    }

    [Fact]
    public void AdoReplayServesRecordedRowsThroughTheConnectStub()
    {
        Instrument.ResetSessionForTest(LoadCapsule(Capsule(
            PgExchange("SELECT id, symbol FROM issuers", new List<object?>
            {
                new Dictionary<string, object?> { ["id"] = 7L, ["symbol"] = "ACME" },
            }))));
        // The inner connection throws on Open: replay must never reach it.
        var connection = Ado.Wrap(new ExplodingConnection());
        connection.Open();
        using var command = connection.CreateCommand();
        command.CommandText = "SELECT id, symbol FROM issuers";
        using var reader = command.ExecuteReader();
        Assert.True(reader.Read());
        Assert.Equal(7L, reader.GetValue(reader.GetOrdinal("id")));
        Assert.Equal("ACME", reader.GetString(reader.GetOrdinal("symbol")));
        Assert.False(reader.Read());
    }

    [Fact]
    public void AnAdoStatementTheCaptureNeverSawFailsClosed()
    {
        Instrument.ResetSessionForTest(LoadCapsule(Capsule(
            PgExchange("SELECT 1", new List<object?>()))));
        var connection = Ado.Wrap(new ExplodingConnection());
        connection.Open();
        using var command = connection.CreateCommand();
        command.CommandText = "SELECT something else";
        var original = Console.Error;
        Console.SetError(new StringWriter());
        try
        {
            Assert.Throws<Ado.DivergedException>(() => command.ExecuteReader());
        }
        finally
        {
            Console.SetError(original);
        }
    }

    [Fact]
    public void ARecordedDbErrorReThrowsAtReplay()
    {
        var exchange = new Dictionary<string, object?>
        {
            ["protocol"] = "pg",
            ["request"] = new Dictionary<string, object?> { ["text"] = "SELECT boom" },
            ["response"] = new Dictionary<string, object?>
            {
                ["error"] = new Dictionary<string, object?>
                {
                    ["message"] = "relation does not exist",
                    ["code"] = "42P01",
                },
            },
        };
        Instrument.ResetSessionForTest(LoadCapsule(Capsule(exchange)));
        var connection = Ado.Wrap(new ExplodingConnection());
        connection.Open();
        using var command = connection.CreateCommand();
        command.CommandText = "SELECT boom";
        var thrown = Assert.Throws<Ado.ReplayedException>(() => command.ExecuteReader());
        Assert.Equal("relation does not exist", thrown.Message);
        Assert.Equal("42P01", thrown.SqlState);
    }

    // --- envelope pins ------------------------------------------------------------------

    [Fact]
    public void TheSeededRandomSourceIsDeterministicInReplayMode()
    {
        Instrument.ResetSessionForTest(LoadCapsule(Capsule()));
        var first = Instrument.RandomSource.NextDouble();
        Instrument.ResetSessionForTest(LoadCapsule(Capsule()));
        var second = Instrument.RandomSource.NextDouble();
        Assert.Equal(first, second);
        Assert.InRange(first, 0, 1);
        // Next() routes through the same seeded stream (Sample override).
        Instrument.ResetSessionForTest(LoadCapsule(Capsule()));
        var left = Instrument.RandomSource.Next(1000);
        Instrument.ResetSessionForTest(LoadCapsule(Capsule()));
        var right = Instrument.RandomSource.Next(1000);
        Assert.Equal(left, right);
    }

    [Fact]
    public void TheTimeProviderPinsToTheCaptureMoment()
    {
        Instrument.ResetSessionForTest(LoadCapsule(Capsule()));
        var now = Instrument.Time.GetUtcNow();
        var observed = DateTimeOffset.FromUnixTimeMilliseconds(1753747200000L);
        // The pinned clock still advances (it is an offset, not a freeze), so it reads
        // within a small window of the capture moment.
        Assert.InRange((now - observed).Duration(), TimeSpan.Zero, TimeSpan.FromMinutes(1));
    }

    [Fact]
    public void OutsideReplayTheExposedSourcesAreTheSystemOnes()
    {
        Instrument.ResetSessionForTest(null);
        Assert.Same(Random.Shared, Instrument.RandomSource);
        Assert.Same(TimeProvider.System, Instrument.Time);
    }

    // --- fake ADO.NET driver (test double; the fixture uses the same idiom) -------------

    private sealed class FakeConnection : DbConnection
    {
        private readonly Dictionary<string, object?>[] _rows;
        private ConnectionState _state = ConnectionState.Closed;

        internal FakeConnection(Dictionary<string, object?>[] rows)
        {
            _rows = rows;
        }

        [System.Diagnostics.CodeAnalysis.AllowNull]
        public override string ConnectionString { get; set; } = string.Empty;
        public override string Database => "fake";
        public override string DataSource => "fake";
        public override string ServerVersion => "0";
        public override ConnectionState State => _state;
        public override void Open() => _state = ConnectionState.Open;
        public override void Close() => _state = ConnectionState.Closed;
        public override void ChangeDatabase(string databaseName) {}
        protected override DbCommand CreateDbCommand() => new FakeCommand(_rows);
        protected override DbTransaction BeginDbTransaction(IsolationLevel level) =>
            throw new NotSupportedException();
    }

    private sealed class FakeCommand : DbCommand
    {
        private readonly Dictionary<string, object?>[] _rows;
        private readonly FakeParameterCollection _parameters = new();

        internal FakeCommand(Dictionary<string, object?>[] rows)
        {
            _rows = rows;
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
        public override int ExecuteNonQuery() => _rows.Length;
        public override object? ExecuteScalar() => _rows.FirstOrDefault()?.Values.First();
        protected override DbDataReader ExecuteDbDataReader(CommandBehavior behavior) =>
            new FakeReader(_rows);
    }

    private sealed class FakeParameter : DbParameter
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

    private sealed class FakeParameterCollection : DbParameterCollection
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
        public override System.Collections.IEnumerator GetEnumerator() =>
            _items.GetEnumerator();
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

    private sealed class FakeReader : DbDataReader
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
        public override System.Collections.IEnumerator GetEnumerator() =>
            new DbEnumerator(this);
    }

    // A driver that must never be reached during hermetic replay.
    private sealed class ExplodingConnection : DbConnection
    {
        [System.Diagnostics.CodeAnalysis.AllowNull]
        public override string ConnectionString { get; set; } = string.Empty;
        public override string Database => "down";
        public override string DataSource => "down";
        public override string ServerVersion =>
            throw new InvalidOperationException("live database reached during replay");
        public override ConnectionState State => ConnectionState.Closed;
        public override void Open() =>
            throw new InvalidOperationException("live database dialed during replay");
        public override void Close() {}
        public override void ChangeDatabase(string databaseName) {}
        protected override DbCommand CreateDbCommand() => new FakeCommand(
            Array.Empty<Dictionary<string, object?>>());
        protected override DbTransaction BeginDbTransaction(IsolationLevel level) =>
            throw new NotSupportedException();
    }
}
