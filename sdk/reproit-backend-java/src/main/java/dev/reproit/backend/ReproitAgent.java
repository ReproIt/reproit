/*
 * OPTIONAL java.lang.instrument agent: AUTOMATIC outbound capture with no
 * hand-instrumentation of each call. An app that adds
 * `-javaagent:reproit-backend-java.jar` gets its outbound JDBC captured onto
 * the ambient request trace (the servlet filter scopes it, or Instrument.scope
 * does) through the SAME Instrument/Exchange/BackendTrace path the opt-in
 * wrappers use, so the recorded shape is byte-identical either way.
 *
 * WHAT THIS AGENT AUTO-CAPTURES (proven by AgentAutoCaptureTest, no wrapper):
 *
 *   JDBC. ByteBuddy weaves `java.sql.Driver.connect` on every driver
 *   implementation (H2, PostgreSQL, MySQL, ...). Driver classes load in the
 *   APP classloader, the same one that loads dev.reproit.backend, so the woven
 *   advice sees these classes with no bootstrap gymnastics. On exit the advice
 *   replaces the returned Connection with ReproitJdbc.wrap(connection): from
 *   then on every createStatement / prepareStatement returns the existing
 *   recording proxy, and every executeQuery / executeUpdate records the pg wire
 *   shape onto the ambient trace. A plain `DriverManager.getConnection(url)` is
 *   now recorded with zero code changes in the app.
 *
 * WHAT STAYS OPT-IN, and why (a NAMED gap, never a silent downgrade):
 *
 *   Outbound HTTP. The only outbound HTTP surface in a dependency-free app is
 *   java.net.http.HttpClient, whose concrete type jdk.internal.net.http
 *   .HttpClientImpl loads in the BOOTSTRAP class loader inside the sealed
 *   java.net.http module. To weave it the advice must reach dev.reproit.backend
 *   from the bootstrap loader (appendToBootstrapClassLoaderSearch of a real SDK
 *   jar) AND the java.net.http module must be opened to that code. A self-attach
 *   run whose SDK classes sit in a classpath DIRECTORY, not a jar, cannot append
 *   to the bootstrap search, so the JDK HTTP client is not woven here. HTTP
 *   capture therefore stays on the library-layer boundary that already works
 *   and is tested: `ReproitHttpClient.wrap(client)` or `Instrument.Http.send`.
 *   The same limitation is documented in README.md.
 *
 * REPLAY. The agent is a CAPTURE mechanism. Hermetic replay (REPROIT_REPLAY)
 * stays on the opt-in boundary (ReproitJdbc.connect returns the in-process stub
 * with the database down; ReproitHttpClient serves recorded exchanges), because
 * serving without dialing the real dependency must run BEFORE the driver
 * connects, which the connect-exit hook cannot do. Running the agent while
 * replaying wraps a live connection, the same named gap as a bare
 * DriverManager.getConnection at replay.
 *
 * FAIL CLOSED. Every advice body suppresses Throwable and the dispatcher wraps
 * its work in try/catch: an instrumentation defect counts a failed capture and
 * returns the host's own value, it never breaks the host call.
 */
package dev.reproit.backend;

import static net.bytebuddy.matcher.ElementMatchers.isInterface;
import static net.bytebuddy.matcher.ElementMatchers.isSubTypeOf;
import static net.bytebuddy.matcher.ElementMatchers.nameStartsWith;
import static net.bytebuddy.matcher.ElementMatchers.named;
import static net.bytebuddy.matcher.ElementMatchers.not;
import static net.bytebuddy.matcher.ElementMatchers.returns;
import static net.bytebuddy.matcher.ElementMatchers.takesArgument;

import java.lang.instrument.Instrumentation;
import java.sql.Connection;
import java.sql.Driver;
import net.bytebuddy.agent.builder.AgentBuilder;
import net.bytebuddy.asm.Advice;

public final class ReproitAgent {
    private ReproitAgent() {}

    /** JVM entry point for `-javaagent:reproit-backend-java.jar`. */
    public static void premain(String arguments, Instrumentation instrumentation) {
        install(instrumentation);
    }

    /** JVM entry point for dynamic attach after startup (Agent-Class). */
    public static void agentmain(String arguments, Instrumentation instrumentation) {
        install(instrumentation);
    }

    /**
     * Install the weaving on `instrumentation`. Package-visible so the agent's
     * own test can drive it with a self-attached Instrumentation. Idempotent
     * enough for tests: ByteBuddy skips a type already carrying the advice.
     */
    static void install(Instrumentation instrumentation) {
        new AgentBuilder.Default()
            // Retransform an already-loaded driver, so attach order does not
            // matter; class-format is unchanged (advice only), so this is safe.
            .with(AgentBuilder.RedefinitionStrategy.RETRANSFORMATION)
            .with(AgentBuilder.TypeStrategy.Default.REDEFINE)
            // Never weave the SDK, ByteBuddy, or JDK-internal types.
            .ignore(nameStartsWith("net.bytebuddy.")
                .or(nameStartsWith("dev.reproit.backend."))
                .or(nameStartsWith("jdk.internal.")))
            .type(isSubTypeOf(Driver.class).and(not(isInterface())))
            .transform((builder, type, loader, module, protection) ->
                builder.visit(Advice.to(DriverConnectAdvice.class).on(
                    named("connect")
                        .and(takesArgument(0, String.class))
                        .and(returns(Connection.class)))))
            .installOn(instrumentation);
    }

    /**
     * Wrap a freshly opened connection so its statements record. Fail closed:
     * a wrap defect counts and returns the untouched connection. Null is a
     * non-match from a driver DriverManager is probing; it passes straight
     * through.
     */
    public static Connection onConnect(Connection connection) {
        if (connection == null) {
            return null;
        }
        try {
            return ReproitJdbc.wrap(connection);
        } catch (RuntimeException defect) {
            Instrument.countFailedCapture();
            return connection;
        }
    }

    /**
     * Woven into every `Driver.connect`: the returned live connection becomes a
     * recording connection. suppress = Throwable so an advice fault can never
     * reach the host driver.
     */
    public static final class DriverConnectAdvice {
        private DriverConnectAdvice() {}

        @Advice.OnMethodExit(suppress = Throwable.class)
        public static void exit(@Advice.Return(readOnly = false) Connection connection) {
            connection = ReproitAgent.onConnect(connection);
        }
    }
}
