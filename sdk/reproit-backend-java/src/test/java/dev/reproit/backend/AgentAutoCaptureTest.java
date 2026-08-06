// Agent auto-capture: the ReproitAgent, self-attached to this JVM, weaves
// java.sql.Driver.connect so a PLAIN DriverManager.getConnection (no
// ReproitJdbc.wrap, no Instrument.Db.run) records its statements onto the
// ambient trace through the existing recording path. This is real bytecode
// instrumentation: ByteBuddyAgent.install() hands a live Instrumentation, the
// agent transforms the H2 driver at load, and the H2 connection comes straight
// from DriverManager with no hand-instrumentation anywhere in the call.
package dev.reproit.backend;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.lang.reflect.Proxy;
import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.ResultSet;
import java.sql.Statement;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import net.bytebuddy.agent.ByteBuddyAgent;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;

class AgentAutoCaptureTest {
    private static final TraceContext CAPTURE_CONTEXT =
        new TraceContext("cap-agent", null, 0, null, null, true);

    @BeforeAll
    static void attachAgent() throws Exception {
        // Self-attach: install the agent onto the running JVM, then load the
        // driver so it is woven at load time.
        ReproitAgent.install(ByteBuddyAgent.install());
        Class.forName("org.h2.Driver");
    }

    @AfterEach
    void clearSession() {
        Instrument.resetSessionForTest(null);
    }

    private static BackendTrace trace() {
        return BackendTrace.begin(CAPTURE_CONTEXT, "GET /quote", new BackendTrace.Options());
    }

    @SuppressWarnings("unchecked")
    private static List<Map<String, Object>> exchangesOf(BackendTrace trace) {
        List<Map<String, Object>> found = new ArrayList<>();
        for (Map<String, Object> event : trace.events()) {
            if (event.get("exchange") instanceof Map<?, ?> exchange) {
                found.add((Map<String, Object>) exchange);
            }
        }
        return found;
    }

    @Test
    void plainJdbcStatementsAutoCaptureWithoutAnyWrapping() throws Exception {
        BackendTrace trace = trace();
        Instrument.scope(trace, () -> {
            // No ReproitJdbc.connect, no wrap: a plain DriverManager call. The
            // agent's woven Driver.connect returns the recording connection.
            Connection connection = DriverManager.getConnection(
                "jdbc:h2:mem:agentauto;DB_CLOSE_DELAY=-1");
            assertTrue(
                Proxy.isProxyClass(connection.getClass()),
                "the agent must weave Driver.connect to return a recording connection");
            Statement statement = connection.createStatement();
            statement.executeUpdate("CREATE TABLE issuers (id INT, symbol VARCHAR)");
            statement.executeUpdate("INSERT INTO issuers VALUES (7, 'ACME')");
            // Quoted aliases pin the column labels: H2 upper-cases bare names,
            // and the label is what the recorded row keys on.
            ResultSet rows = statement.executeQuery(
                "SELECT id AS \"id\", symbol AS \"symbol\" FROM issuers WHERE symbol = 'ACME'");
            assertTrue(rows.next());
            assertEquals(7L, rows.getLong("id"));
            assertEquals("ACME", rows.getString("symbol"));
            assertFalse(rows.next());
            connection.close();
            return null;
        });

        List<Map<String, Object>> exchanges = exchangesOf(trace);
        // CREATE and INSERT (writes) plus the SELECT (read): three recorded pg
        // exchanges, the identical shape the opt-in wrappers produce.
        assertEquals(3, exchanges.size(),
            "every executed statement must record onto the ambient trace");
        for (Map<String, Object> exchange : exchanges) {
            assertEquals("pg", exchange.get("protocol"));
        }
        Map<?, ?> select = exchanges.get(2);
        Map<?, ?> request = (Map<?, ?>) select.get("request");
        assertEquals(
            "SELECT id AS \"id\", symbol AS \"symbol\" FROM issuers WHERE symbol = 'ACME'",
            request.get("text"));
        Map<?, ?> response = (Map<?, ?>) select.get("response");
        assertEquals("SELECT", response.get("command"));
        assertEquals(1L, response.get("rowCount"));
        assertEquals(List.of(Map.of("id", 7, "symbol", "ACME")), response.get("rows"));
        // The SELECT is a read so state oracles keep their meaning.
        String selectEffect = trace.events().stream()
            .filter(event -> event.containsKey("exchange"))
            .reduce((first, second) -> second)
            .orElseThrow()
            .get("effect")
            .toString();
        assertEquals("read", selectEffect);
    }

    @Test
    void anUnscopedAgentCaptureRecordsNothingRatherThanHalfRecording() throws Exception {
        // The agent is installed, but no trace is ambient: the woven connection
        // records nothing, exactly like the opt-in boundary off a trace.
        BackendTrace trace = trace();
        Connection connection = DriverManager.getConnection(
            "jdbc:h2:mem:agentauto;DB_CLOSE_DELAY=-1");
        Statement statement = connection.createStatement();
        statement.executeUpdate("CREATE TABLE unscoped (id INT)");
        statement.executeUpdate("INSERT INTO unscoped VALUES (1)");
        connection.close();
        assertNotNull(trace);
        assertTrue(exchangesOf(trace).isEmpty(),
            "a call with no ambient trace must record nothing");
    }
}
