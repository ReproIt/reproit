/*
 * JUnit 5 extension for CI capture mode: `@ExtendWith(ReproitCi.class)` on a
 * test class gives every test the flaky-CI wedge semantics of {@link Ci}:
 *
 * - `REPROIT_CI_CAPTURE=1`: each test runs inside its own trace (suite = the
 *   test class's simple name, test = the method name), the wrapped outbound
 *   clients record exchanges plus the determinism envelope, and a FAILING
 *   test spools a version-2 capsule to the bounded spool before the failure
 *   reaches JUnit untouched.
 * - `REPROIT_REPLAY=<capsule>`: only the capsule's named test runs (all
 *   others are disabled with the target named), the SDK serves the recorded
 *   exchanges in process, and the observed result is reported as the
 *   `REPROIT:CI-TEST` stderr marker `reproit check` parses.
 * - Neither env: the extension is inert and JUnit is untouched.
 *
 * JUnit is a `provided` dependency of this SDK: the extension only loads in
 * suites that already have JUnit 5 on the classpath, and the SDK jar stays
 * zero-dependency for applications.
 */
package dev.reproit.backend;

import java.lang.reflect.Method;
import org.junit.jupiter.api.extension.ConditionEvaluationResult;
import org.junit.jupiter.api.extension.ExecutionCondition;
import org.junit.jupiter.api.extension.ExtensionContext;
import org.junit.jupiter.api.extension.InvocationInterceptor;
import org.junit.jupiter.api.extension.ReflectiveInvocationContext;

public final class ReproitCi implements InvocationInterceptor, ExecutionCondition {
    @Override
    public ConditionEvaluationResult evaluateExecutionCondition(ExtensionContext context) {
        if (Ci.mode() != Ci.Mode.REPLAY || context.getTestMethod().isEmpty()) {
            return ConditionEvaluationResult.enabled("reproit ci not replaying");
        }
        String target = Ci.replayTarget();
        String operation = operationOf(context);
        return operation.equals(target)
            ? ConditionEvaluationResult.enabled("reproit replay target")
            : ConditionEvaluationResult.disabled("reproit replay targets " + target);
    }

    @Override
    public void interceptTestMethod(
            Invocation<Void> invocation,
            ReflectiveInvocationContext<Method> invocationContext,
            ExtensionContext extensionContext) throws Throwable {
        String suite = suiteOf(extensionContext);
        String test = extensionContext.getRequiredTestMethod().getName();
        switch (Ci.mode()) {
            case CAPTURE -> Ci.captureRun(suite, test, invocation::proceed);
            case REPLAY -> Ci.replayRun(suite, test, invocation::proceed);
            default -> invocation.proceed();
        }
    }

    private static String suiteOf(ExtensionContext context) {
        return context.getRequiredTestClass().getSimpleName();
    }

    private static String operationOf(ExtensionContext context) {
        return Ci.operationFor(
            suiteOf(context), context.getRequiredTestMethod().getName());
    }
}
