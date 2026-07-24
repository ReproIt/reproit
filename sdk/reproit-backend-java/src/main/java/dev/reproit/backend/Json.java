/*
 * Compact JSON for reproit-backend-java, zero dependencies.
 *
 * Writer: compact JSON with recursively sorted object keys, byte-identical to
 * the Rust adapter's serde_json (BTreeMap) encoding of the same events. Only
 * `"`, `\` and control characters are escaped (lowercase u00xx escapes for bare
 * controls); non-ASCII text ships as UTF-8, exactly like the Node and Python
 * ports. Integral numbers print without a decimal point, doubles keep one
 * (serde_json f64 style). Non-finite doubles serialize as null.
 *
 * Reader: minimal strict parser for the servlet filter's bounded JSON bodies
 * and the tests. Objects become LinkedHashMap, arrays ArrayList, integers
 * Long, decimals Double. Depth is capped; input size is bounded by callers.
 */
package dev.reproit.backend;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;

public final class Json {
    static final int MAX_PARSE_DEPTH = 64;

    private Json() {}

    public static String canonicalJson(Object value) {
        StringBuilder out = new StringBuilder();
        write(out, value);
        return out.toString();
    }

    private static void write(StringBuilder out, Object value) {
        if (value == null) {
            out.append("null");
        } else if (value instanceof Boolean bool) {
            out.append(bool ? "true" : "false");
        } else if (value instanceof CharSequence text) {
            writeString(out, text.toString());
        } else if (value instanceof Double || value instanceof Float) {
            double number = ((Number) value).doubleValue();
            out.append(Double.isFinite(number) ? Double.toString(number) : "null");
        } else if (value instanceof Number number) {
            out.append(number.longValue());
        } else if (value instanceof Map<?, ?> map) {
            TreeMap<String, Object> sorted = new TreeMap<>();
            for (Map.Entry<?, ?> entry : map.entrySet()) {
                sorted.put(String.valueOf(entry.getKey()), entry.getValue());
            }
            out.append('{');
            boolean first = true;
            for (Map.Entry<String, Object> entry : sorted.entrySet()) {
                if (!first) out.append(',');
                first = false;
                writeString(out, entry.getKey());
                out.append(':');
                write(out, entry.getValue());
            }
            out.append('}');
        } else if (value instanceof List<?> list) {
            out.append('[');
            for (int index = 0; index < list.size(); index++) {
                if (index > 0) out.append(',');
                write(out, list.get(index));
            }
            out.append(']');
        } else {
            throw new IllegalArgumentException(
                "unsupported json value: " + value.getClass().getName());
        }
    }

    private static void writeString(StringBuilder out, String value) {
        out.append('"');
        for (int index = 0; index < value.length(); index++) {
            char ch = value.charAt(index);
            switch (ch) {
                case '"' -> out.append("\\\"");
                case '\\' -> out.append("\\\\");
                case '\n' -> out.append("\\n");
                case '\r' -> out.append("\\r");
                case '\t' -> out.append("\\t");
                case '\b' -> out.append("\\b");
                case '\f' -> out.append("\\f");
                default -> {
                    if (ch < 0x20) {
                        out.append(String.format("\\u%04x", (int) ch));
                    } else {
                        out.append(ch);
                    }
                }
            }
        }
        out.append('"');
    }

    /** Parse strict JSON text; throws IllegalArgumentException on any defect. */
    public static Object parse(String text) {
        Parser parser = new Parser(text);
        Object value = parser.value(0);
        parser.skipWhitespace();
        if (parser.position != text.length()) throw new IllegalArgumentException("trailing data");
        return value;
    }

    private static final class Parser {
        final String text;
        int position;

        Parser(String text) {
            this.text = text;
        }

        Object value(int depth) {
            if (depth > MAX_PARSE_DEPTH) throw new IllegalArgumentException("too deep");
            skipWhitespace();
            char ch = peek();
            return switch (ch) {
                case '{' -> object(depth);
                case '[' -> array(depth);
                case '"' -> string();
                case 't' -> literal("true", Boolean.TRUE);
                case 'f' -> literal("false", Boolean.FALSE);
                case 'n' -> literal("null", null);
                default -> number();
            };
        }

        Map<String, Object> object(int depth) {
            expect('{');
            Map<String, Object> map = new LinkedHashMap<>();
            skipWhitespace();
            if (peek() == '}') {
                position++;
                return map;
            }
            while (true) {
                skipWhitespace();
                String key = string();
                skipWhitespace();
                expect(':');
                map.put(key, value(depth + 1));
                skipWhitespace();
                char next = next();
                if (next == '}') return map;
                if (next != ',') throw new IllegalArgumentException("expected , or }");
            }
        }

        List<Object> array(int depth) {
            expect('[');
            List<Object> list = new ArrayList<>();
            skipWhitespace();
            if (peek() == ']') {
                position++;
                return list;
            }
            while (true) {
                list.add(value(depth + 1));
                skipWhitespace();
                char next = next();
                if (next == ']') return list;
                if (next != ',') throw new IllegalArgumentException("expected , or ]");
            }
        }

        String string() {
            expect('"');
            StringBuilder out = new StringBuilder();
            while (true) {
                char ch = next();
                if (ch == '"') return out.toString();
                if (ch < 0x20) throw new IllegalArgumentException("bare control character");
                if (ch != '\\') {
                    out.append(ch);
                    continue;
                }
                char escape = next();
                switch (escape) {
                    case '"' -> out.append('"');
                    case '\\' -> out.append('\\');
                    case '/' -> out.append('/');
                    case 'n' -> out.append('\n');
                    case 'r' -> out.append('\r');
                    case 't' -> out.append('\t');
                    case 'b' -> out.append('\b');
                    case 'f' -> out.append('\f');
                    case 'u' -> {
                        if (position + 4 > text.length()) {
                            throw new IllegalArgumentException("bad unicode escape");
                        }
                        String hex = text.substring(position, position + 4);
                        position += 4;
                        out.append((char) Integer.parseInt(hex, 16));
                    }
                    default -> throw new IllegalArgumentException("bad escape: " + escape);
                }
            }
        }

        Object number() {
            int start = position;
            boolean integral = true;
            if (peek() == '-') position++;
            while (position < text.length()) {
                char ch = text.charAt(position);
                if (ch >= '0' && ch <= '9') {
                    position++;
                } else if (ch == '.' || ch == 'e' || ch == 'E' || ch == '+' || ch == '-') {
                    integral = false;
                    position++;
                } else {
                    break;
                }
            }
            String token = text.substring(start, position);
            try {
                return integral ? (Object) Long.parseLong(token) : Double.parseDouble(token);
            } catch (NumberFormatException bad) {
                throw new IllegalArgumentException("bad number: " + token);
            }
        }

        Object literal(String token, Object value) {
            if (!text.startsWith(token, position)) throw new IllegalArgumentException("bad token");
            position += token.length();
            return value;
        }

        void skipWhitespace() {
            while (position < text.length()) {
                char ch = text.charAt(position);
                if (ch != ' ' && ch != '\t' && ch != '\n' && ch != '\r') break;
                position++;
            }
        }

        char peek() {
            if (position >= text.length()) throw new IllegalArgumentException("truncated json");
            return text.charAt(position);
        }

        char next() {
            char ch = peek();
            position++;
            return ch;
        }

        void expect(char wanted) {
            if (next() != wanted) throw new IllegalArgumentException("expected " + wanted);
        }
    }
}
