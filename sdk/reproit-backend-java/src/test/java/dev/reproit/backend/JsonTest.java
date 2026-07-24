// Canonical JSON parity tests. The golden strings below are the exact bytes
// produced by sdk/reproit-backend-node's canonicalJson for the same values
// (verified against the Node SDK; Node, Python, and Rust are byte-identical).
package dev.reproit.backend;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

class JsonTest {
    @Test
    void goldenBytesMatchTheNodeSdk() {
        Map<String, Object> nested = new LinkedHashMap<>();
        nested.put("z", java.util.Arrays.asList(1L, "two", true, null));
        nested.put("quo\"te", "line\nbreak");
        nested.put("num", 2.5);
        Map<String, Object> value = new LinkedHashMap<>();
        value.put("b", 1L);
        value.put("a", nested);
        value.put("emptyObj", Map.of());
        value.put("emptyArr", List.of());
        value.put("big", 9007199254740991L);
        value.put("neg", -42L);
        value.put("uni", "héllo  ");
        String golden = "{\"a\":{\"num\":2.5,\"quo\\\"te\":\"line\\nbreak\","
            + "\"z\":[1,\"two\",true,null]},\"b\":1,\"big\":9007199254740991,"
            + "\"emptyArr\":[],\"emptyObj\":{},\"neg\":-42,\"uni\":\"héllo  \"}";
        assertEquals(golden, Json.canonicalJson(value));
    }

    @Test
    void bareControlCharactersUseLowercaseUnicodeEscapes() {
        // Matches serde_json / JSON.stringify: lowercase u00xx escapes for bare controls.
        assertEquals(
            "{\"ctrl\":\"tab\\t\\u0001\\u001f\"}",
            Json.canonicalJson(Map.of("ctrl", "tab\t\u0001\u001f")));
    }

    @Test
    void nonFiniteDoublesSerializeAsNull() {
        assertEquals("[null,null,0.5]", Json.canonicalJson(
            List.of(Double.NaN, Double.POSITIVE_INFINITY, 0.5)));
    }

    @Test
    void parserRoundTripsCanonicalOutput() {
        String text = "{\"a\":[1,\"two\",{\"deep\":true}],\"b\":null,\"c\":-2.5e2,\"d\":\"q\\\"\"}";
        Object parsed = Json.parse(text);
        assertEquals(
            "{\"a\":[1,\"two\",{\"deep\":true}],\"b\":null,\"c\":-250.0,\"d\":\"q\\\"\"}",
            Json.canonicalJson(parsed));
        assertEquals(1L, ((List<?>) ((Map<?, ?>) parsed).get("a")).get(0));
        assertEquals("snow☃", Json.parse("\"snow\\u2603\""));
    }

    @Test
    void parserRejectsMalformedInput() {
        for (String bad : List.of("", "{", "{\"a\":}", "[1,]", "tru", "\"open", "1 2", "{'a':1}")) {
            assertThrows(IllegalArgumentException.class, () -> Json.parse(bad), bad);
        }
    }
}
