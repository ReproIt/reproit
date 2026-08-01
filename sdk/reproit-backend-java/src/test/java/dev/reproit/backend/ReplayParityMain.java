// Runner for sdk/test/backend_replay_parity_test.js: reads a capsule from
// stdin, serves the recorded SSE exchange, provokes a prompt-drift
// divergence, and prints the byte-compared results (serve, 599 body, the
// REPROIT:DIVERGENCE marker line) as one JSON object on stdout. Compiled by
// the parity test with plain javac against the main sources; no Maven and
// no network involved.
package dev.reproit.backend;

import java.io.ByteArrayOutputStream;
import java.io.PrintStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

public final class ReplayParityMain {
    private ReplayParityMain() {}

    public static void main(String[] args) throws Exception {
        byte[] raw = System.in.readAllBytes();
        Path capsule = Files.createTempFile("reproit-parity-capsule", ".json");
        capsule.toFile().deleteOnExit();
        Files.write(capsule, raw);
        Replay session = Replay.load(capsule.toString());
        if (session == null) throw new IllegalStateException("unloadable capsule");

        Map<String, Object> streamProbe = new LinkedHashMap<>();
        streamProbe.put("method", "GET");
        streamProbe.put("url", "http://llm.internal/stream");
        Replay.Served served = session.serveHttp(streamProbe);

        Map<String, Object> driftProbe = new LinkedHashMap<>();
        driftProbe.put("method", "POST");
        driftProbe.put("url", "http://llm.internal/v1/chat");
        driftProbe.put("body", Json.parse("""
            {"messages":[
              {"role":"user","content":"hello"},
              {"role":"assistant","content":"hi"},
              {"role":"user","content":"DIFFERENT QUESTION"}]}"""));
        PrintStream original = System.err;
        ByteArrayOutputStream held = new ByteArrayOutputStream();
        System.setErr(new PrintStream(held, true, StandardCharsets.UTF_8));
        Replay.Served diverged;
        try {
            diverged = session.serveHttp(driftProbe);
        } finally {
            System.setErr(original);
        }
        String marker = held.toString(StandardCharsets.UTF_8).lines()
            .filter(line -> line.startsWith(Replay.DIVERGENCE_MARKER))
            .findFirst().orElseThrow();

        List<String> chunks = new ArrayList<>();
        for (byte[] chunk : served.chunks()) {
            chunks.add(new String(chunk, StandardCharsets.UTF_8));
        }
        Map<String, Object> serve = new LinkedHashMap<>();
        serve.put("status", (long) served.status());
        serve.put("bodyText", served.bodyText());
        serve.put("chunks", chunks);
        Map<String, Object> result = new LinkedHashMap<>();
        result.put("serve", serve);
        result.put("divergedBody", diverged.bodyText());
        result.put("marker", marker);
        System.out.println(Json.orderedJson(result));
    }
}
