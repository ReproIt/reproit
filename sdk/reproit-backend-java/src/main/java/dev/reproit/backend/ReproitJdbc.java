/*
 * Delegating JDBC wrap: the one canonical DB boundary, like pg in Node and
 * psycopg in Python, emitting exactly the Node `pg` wire shape (request
 * `{text, values}`, response `{command, rowCount, rows}` or `{error:
 * {message, code}}`, rows bounded at 64 with a truncated marker).
 *
 * `ReproitJdbc.connect(live)` is the app's connection point. Capture mode
 * calls `live` and returns a delegating java.sql.Connection (a dynamic
 * proxy) whose Statements and PreparedStatements record every executeQuery /
 * executeUpdate and its result onto the ambient trace; result rows are
 * drained once at execute time and re-served to the app through a recorded
 * ResultSet, so the app sees exactly the rows the driver returned. Replay
 * mode NEVER calls `live`: the returned connection is an in-process stub
 * served from the recorded exchanges, so the app boots and answers with the
 * database down. An unmatched statement throws SQLException after emitting
 * the structured divergence marker; a recorded error re-raises with its
 * SQLState.
 *
 * Bounded on purpose, each a NAMED gap, not a silent downgrade: only
 * executeQuery and executeUpdate on Statement/PreparedStatement with
 * indexed set-parameters are recorded; batch APIs, CallableStatement,
 * generated keys, scrollable/updatable cursors and multi-result execute()
 * pass through unrecorded in capture and fail loudly in replay. Connections
 * the app opens through DriverManager directly are invisible (no weaving).
 */
package dev.reproit.backend;

import java.lang.reflect.InvocationHandler;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.lang.reflect.Proxy;
import java.sql.Connection;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.ResultSetMetaData;
import java.sql.SQLException;
import java.sql.Statement;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;

public final class ReproitJdbc {
    private ReproitJdbc() {}

    /** A live connection source; never invoked in replay mode. */
    public interface ConnectionSource {
        Connection open() throws Exception;
    }

    /**
     * The boundary. Replay mode returns the in-process stub (the database
     * may be down or absent); otherwise `live` opens the real connection,
     * returned wrapped so statements record.
     */
    public static Connection connect(ConnectionSource live) throws SQLException {
        Replay session = Instrument.session();
        if (session != null) return replayConnection(session);
        try {
            return wrap(live.open());
        } catch (SQLException failure) {
            throw failure;
        } catch (Exception failure) {
            throw new SQLException(String.valueOf(failure.getMessage()), failure);
        }
    }

    /** Wrap a live connection so its statements record. Idempotent. */
    public static Connection wrap(Connection delegate) {
        boolean wrapped = Proxy.isProxyClass(delegate.getClass())
            && Proxy.getInvocationHandler(delegate) instanceof RecordingConnection;
        if (wrapped) return delegate;
        return (Connection) Proxy.newProxyInstance(
            ReproitJdbc.class.getClassLoader(),
            new Class<?>[] {Connection.class},
            new RecordingConnection(delegate));
    }

    // ------------------------------------------------------------------
    // Capture side: delegating proxies that record.
    // ------------------------------------------------------------------

    private static final class RecordingConnection implements InvocationHandler {
        private final Connection delegate;

        RecordingConnection(Connection delegate) {
            this.delegate = delegate;
        }

        @Override
        public Object invoke(Object proxy, Method method, Object[] args) throws Throwable {
            Object result = call(delegate, method, args);
            String name = method.getName();
            if (name.equals("prepareStatement") && args != null && args.length >= 1
                    && args[0] instanceof String sql
                    && result instanceof PreparedStatement statement) {
                return Proxy.newProxyInstance(
                    ReproitJdbc.class.getClassLoader(),
                    new Class<?>[] {PreparedStatement.class},
                    new RecordingStatement(statement, sql));
            }
            if (name.equals("createStatement") && result instanceof Statement statement) {
                return Proxy.newProxyInstance(
                    ReproitJdbc.class.getClassLoader(),
                    new Class<?>[] {Statement.class},
                    new RecordingStatement(statement, null));
            }
            return result;
        }
    }

    private static final class RecordingStatement implements InvocationHandler {
        private final Statement delegate;
        private final String preparedSql;
        private final List<Object> values = new ArrayList<>();

        RecordingStatement(Statement delegate, String preparedSql) {
            this.delegate = delegate;
            this.preparedSql = preparedSql;
        }

        @Override
        public Object invoke(Object proxy, Method method, Object[] args) throws Throwable {
            String name = method.getName();
            if (name.startsWith("set") && args != null && args.length >= 2
                    && args[0] instanceof Integer index && index >= 1) {
                // Indexed parameter: remember it for the probe; setNull and
                // friends record a null value.
                while (values.size() < index) values.add(null);
                values.set(index - 1, name.equals("setNull") ? null : args[1]);
                return call(delegate, method, args);
            }
            boolean query = name.equals("executeQuery");
            boolean update = name.equals("executeUpdate");
            if (!query && !update) return call(delegate, method, args);
            String text = preparedSql != null ? preparedSql
                : (args != null && args.length >= 1 && args[0] instanceof String sql
                    ? sql : null);
            if (text == null) return call(delegate, method, args);
            List<Object> params = preparedSql != null ? new ArrayList<>(values) : List.of();
            BackendTrace trace = Instrument.ambient();
            Object result;
            try {
                result = call(delegate, method, args);
            } catch (SQLException failure) {
                if (trace != null) {
                    record(trace, text, params, Exchange.dbError(
                        String.valueOf(failure.getMessage()), failure.getSQLState()));
                }
                throw failure;
            }
            if (trace == null) return result;
            if (query && result instanceof ResultSet rows) {
                List<Map<String, Object>> drained = drain(rows);
                record(trace, text, params, Exchange.dbOutcome(
                    commandTag(text), drained.size(), new ArrayList<Object>(drained)));
                return recordedResultSet(drained);
            }
            if (update) {
                long count = ((Number) result).longValue();
                record(trace, text, params,
                    Exchange.dbOutcome(commandTag(text), count, List.of()));
            }
            return result;
        }
    }

    private static void record(
            BackendTrace trace, String text, List<Object> values, Map<String, Object> outcome) {
        try {
            trace.effect(Exchange.dbEffectKind(text), new BackendTrace.Effect()
                .resource("pg")
                .key(text.substring(0, Math.min(text.length(), 256)))
                .exchange(Exchange.db(text, values, outcome)));
            Instrument.countCapturedExchange();
        } catch (RuntimeException ignored) {
            // The trace may have finished or overflowed; the host call goes on.
            Instrument.countFailedCapture();
        }
    }

    /** Rows as the Node pg driver records them: one object per row. */
    private static List<Map<String, Object>> drain(ResultSet rows) throws SQLException {
        ResultSetMetaData meta = rows.getMetaData();
        int columns = meta.getColumnCount();
        List<String> labels = new ArrayList<>(columns);
        for (int index = 1; index <= columns; index++) {
            labels.add(meta.getColumnLabel(index));
        }
        List<Map<String, Object>> drained = new ArrayList<>();
        while (rows.next()) {
            Map<String, Object> row = new LinkedHashMap<>();
            for (int index = 1; index <= columns; index++) {
                row.put(labels.get(index - 1), rows.getObject(index));
            }
            drained.add(row);
        }
        rows.close();
        return drained;
    }

    /** The pg command tag for a statement: its leading verb, uppercased. */
    static String commandTag(String text) {
        String stripped = text.stripLeading();
        int end = 0;
        while (end < stripped.length() && Character.isLetter(stripped.charAt(end))) end += 1;
        return end == 0 ? null : stripped.substring(0, end).toUpperCase(Locale.ROOT);
    }

    private static Object call(Object target, Method method, Object[] args) throws Throwable {
        try {
            return method.invoke(target, args);
        } catch (InvocationTargetException unwrapped) {
            throw unwrapped.getCause();
        }
    }

    // ------------------------------------------------------------------
    // Replay side: in-process stubs served from the capture.
    // ------------------------------------------------------------------

    /** The connect stub: the app boots with the database down. */
    static Connection replayConnection(Replay session) {
        return (Connection) Proxy.newProxyInstance(
            ReproitJdbc.class.getClassLoader(),
            new Class<?>[] {Connection.class},
            (proxy, method, args) -> {
                String name = method.getName();
                if (name.equals("prepareStatement") && args != null && args.length >= 1
                        && args[0] instanceof String sql) {
                    return replayStatement(session, sql);
                }
                if (name.equals("createStatement")) return replayStatement(session, null);
                return switch (name) {
                    case "close", "commit", "rollback", "setAutoCommit",
                        "setReadOnly", "setTransactionIsolation", "abort" -> null;
                    case "getAutoCommit" -> Boolean.TRUE;
                    case "isClosed", "isReadOnly" -> Boolean.FALSE;
                    case "isValid" -> Boolean.TRUE;
                    case "toString" -> "ReproitJdbc.replayConnection";
                    case "hashCode" -> System.identityHashCode(proxy);
                    case "equals" -> proxy == args[0];
                    default -> throw new SQLException(
                        "reproit replay: unsupported Connection method " + name);
                };
            });
    }

    private static Statement replayStatement(Replay session, String preparedSql) {
        Class<?> face = preparedSql != null ? PreparedStatement.class : Statement.class;
        List<Object> values = new ArrayList<>();
        return (Statement) Proxy.newProxyInstance(
            ReproitJdbc.class.getClassLoader(),
            new Class<?>[] {face},
            (proxy, method, args) -> {
                String name = method.getName();
                if (name.startsWith("set") && args != null && args.length >= 2
                        && args[0] instanceof Integer index && index >= 1) {
                    while (values.size() < index) values.add(null);
                    values.set(index - 1, name.equals("setNull") ? null : args[1]);
                    return null;
                }
                boolean query = name.equals("executeQuery");
                boolean update = name.equals("executeUpdate");
                if (query || update) {
                    String text = preparedSql != null ? preparedSql
                        : (args != null && args.length >= 1 && args[0] instanceof String sql
                            ? sql : null);
                    if (text == null) {
                        throw new SQLException("reproit replay: statement text required");
                    }
                    Map<String, Object> outcome =
                        serve(session, text, new ArrayList<>(values));
                    if (query) return recordedResultSet(rowsOf(outcome));
                    return outcome.get("rowCount") instanceof Number number
                        ? number.intValue() : 0;
                }
                return switch (name) {
                    case "close", "cancel", "clearParameters", "setFetchSize",
                        "setMaxRows", "setQueryTimeout" -> null;
                    case "isClosed" -> Boolean.FALSE;
                    case "getConnection" -> throw new SQLException(
                        "reproit replay: getConnection is not served");
                    case "toString" -> "ReproitJdbc.replayStatement";
                    case "hashCode" -> System.identityHashCode(proxy);
                    case "equals" -> proxy == args[0];
                    default -> throw new SQLException(
                        "reproit replay: unsupported Statement method " + name);
                };
            });
    }

    /** Match one statement; throws on divergence or a recorded error. */
    private static Map<String, Object> serve(
            Replay session, String text, List<Object> values) throws SQLException {
        Map<String, Object> probe = new LinkedHashMap<>();
        probe.put("text", text);
        if (!values.isEmpty()) probe.put("values", values);
        Map<String, Object> recorded = session.matched("pg", probe);
        if (recorded == null) {
            throw new SQLException("reproit: pg call diverged from the capture");
        }
        Map<String, Object> outcome = recorded.get("response") instanceof Map<?, ?> map
            ? castMap(map) : Map.of();
        if (outcome.get("error") instanceof Map<?, ?> error) {
            Object message = error.get("message");
            Object code = error.get("code");
            throw new SQLException(
                message == null ? "recorded pg error" : String.valueOf(message),
                code == null ? null : String.valueOf(code));
        }
        return outcome;
    }

    private static List<Map<String, Object>> rowsOf(Map<String, Object> outcome) {
        List<Map<String, Object>> rows = new ArrayList<>();
        if (outcome.get("rows") instanceof List<?> recorded) {
            for (Object row : recorded) {
                if (row instanceof Map<?, ?> map) rows.add(castMap(map));
            }
        }
        return rows;
    }

    /**
     * A forward-only ResultSet over drained (or recorded) rows, public so a
     * fixture faking a driver can return one: next, the
     * getObject/getString/getInt/getLong/getBoolean/getDouble surface by
     * index or label, wasNull, and metadata. Anything else fails loudly.
     */
    public static ResultSet recordedResultSet(List<Map<String, Object>> rows) {
        return (ResultSet) Proxy.newProxyInstance(
            ReproitJdbc.class.getClassLoader(),
            new Class<?>[] {ResultSet.class},
            new RecordedRows(rows));
    }

    private static final class RecordedRows implements InvocationHandler {
        private final List<Map<String, Object>> rows;
        private int at = -1;
        private boolean lastWasNull = false;
        private boolean closed = false;

        RecordedRows(List<Map<String, Object>> rows) {
            this.rows = rows;
        }

        private Object valueAt(Object[] args) throws SQLException {
            if (at < 0 || at >= rows.size()) {
                throw new SQLException("reproit: no current row");
            }
            Map<String, Object> row = rows.get(at);
            Object value;
            if (args[0] instanceof Integer index) {
                List<Object> ordered = new ArrayList<>(row.values());
                if (index < 1 || index > ordered.size()) {
                    throw new SQLException("reproit: column index " + index);
                }
                value = ordered.get(index - 1);
            } else {
                String label = String.valueOf(args[0]);
                if (!row.containsKey(label)) {
                    throw new SQLException("reproit: unknown column " + label);
                }
                value = row.get(label);
            }
            lastWasNull = value == null;
            return value;
        }

        @Override
        public Object invoke(Object proxy, Method method, Object[] args) throws Throwable {
            String name = method.getName();
            switch (name) {
                case "next":
                    at += 1;
                    return at < rows.size();
                case "wasNull":
                    return lastWasNull;
                case "close":
                    closed = true;
                    return null;
                case "isClosed":
                    return closed;
                case "getMetaData":
                    return metaDataOf(rows);
                case "getObject":
                    return valueAt(args);
                case "getString": {
                    Object value = valueAt(args);
                    return value == null ? null : String.valueOf(value);
                }
                case "getInt": {
                    Object value = valueAt(args);
                    return value instanceof Number number ? number.intValue() : 0;
                }
                case "getLong": {
                    Object value = valueAt(args);
                    return value instanceof Number number ? number.longValue() : 0L;
                }
                case "getDouble": {
                    Object value = valueAt(args);
                    return value instanceof Number number ? number.doubleValue() : 0.0;
                }
                case "getBoolean":
                    return Boolean.TRUE.equals(valueAt(args));
                case "toString":
                    return "ReproitJdbc.recordedResultSet";
                case "hashCode":
                    return System.identityHashCode(proxy);
                case "equals":
                    return proxy == args[0];
                default:
                    throw new SQLException(
                        "reproit: unsupported ResultSet method " + name);
            }
        }
    }

    private static ResultSetMetaData metaDataOf(List<Map<String, Object>> rows) {
        List<String> labels = rows.isEmpty()
            ? List.of() : new ArrayList<>(rows.get(0).keySet());
        return (ResultSetMetaData) Proxy.newProxyInstance(
            ReproitJdbc.class.getClassLoader(),
            new Class<?>[] {ResultSetMetaData.class},
            (proxy, method, args) -> switch (method.getName()) {
                case "getColumnCount" -> labels.size();
                case "getColumnLabel", "getColumnName" -> labels.get((Integer) args[0] - 1);
                case "toString" -> "ReproitJdbc.metaData";
                case "hashCode" -> System.identityHashCode(proxy);
                case "equals" -> proxy == args[0];
                default -> throw new SQLException(
                    "reproit: unsupported ResultSetMetaData method " + method.getName());
            });
    }

    @SuppressWarnings("unchecked")
    private static Map<String, Object> castMap(Map<?, ?> map) {
        return (Map<String, Object>) map;
    }
}
