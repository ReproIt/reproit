#!/usr/bin/env bash
# Linux AT-SPI multi-actor scenario RUNTIME e2e. Validates the conductor client
# in runners/linux-atspi.py (run_scenario_actor + the /claim | /next | /done
# barrier protocol) with a REAL two-actor run: two runner processes, each
# spawning and pid-binding its OWN instance of a small GTK4 fixture, pull an
# interleaved script from a stub conductor speaking the exact modes/barrier.rs
# wire protocol (the same stub pattern as runners/rn/scenario.test.mjs), all
# inside an ubuntu:24.04 container with Xvfb + a session bus + the a11y bus.
#
# WHAT IT PROVES:
#   * /claim hands out a role while REPROIT_DEVICE pins the other (env wins);
#   * strict global interleaving: every step served AND acked on the wire in
#     the script's exact order (one ACT outstanding at a time);
#   * per-instance isolation via pid-bound app discovery: actor a's taps move
#     ONLY a's counter (a sees "Count: 2", b sees "Count: 1", and b's typed
#     text never appears on a's instance);
#   * a real tap: do_action(0) on a live GtkButton through AT-SPI;
#   * a real type: an EditableText set_text_contents that fires the app's own
#     changed handler (asserted via the echoed label, not the entry contents);
#   * assert:text / assert:count pass against live snapshots, both actors ack
#     DONE and finish "All tests passed" with no FUZZ:MISS and no crash block.
# WHAT IT DOES NOT PROVE: the Rust orchestrator spawn path and the real
# modes/barrier.rs conductor (the stub mirrors its wire contract), and the
# bus-vanish crash oracle (the fixture never dies here).
#
# Deliberately NOT wired into ci.yml: it needs docker-in-runner (image build +
# a privileged-ish container per run), a cost decision taken separately. Run
# locally or in a manual workflow: bash .github/scripts/atspi-scenario-e2e.sh
#
# Needs: docker.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
IMAGE=reproit-atspi-scenario-e2e

cat > "$WORK/Dockerfile" <<'EOF'
FROM rust:1.88-bookworm AS rust-toolchain
FROM ubuntu:24.04
COPY --from=rust-toolchain /usr/local/cargo /usr/local/cargo
COPY --from=rust-toolchain /usr/local/rustup /usr/local/rustup
ENV PATH=/usr/local/cargo/bin:$PATH CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    gcc pkg-config libgtk-4-dev \
    xvfb dbus at-spi2-core \
    libatspi2.0-0 libatspi2.0-dev python3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
EOF

# The fixture: one window per process instance. An "Increment" button drives a
# per-instance "Count: N" label (the isolation probe); an entry (accessible
# label "msg") echoes its contents into an "Echo: ..." label, so a passing
# echo assert proves the EditableText write ran the app's change handler.
# G_APPLICATION_NON_UNIQUE so two instances coexist on one session bus.
cat > "$WORK/fixture.c" <<'EOF'
#include <gtk/gtk.h>

static int count = 0;
static GtkWidget *counter_label;
static GtkWidget *echo_label;

static void on_inc(GtkButton *b, gpointer u) {
    char buf[64];
    (void)b; (void)u;
    g_snprintf(buf, sizeof buf, "Count: %d", ++count);
    gtk_label_set_text(GTK_LABEL(counter_label), buf);
}

static void on_changed(GtkEditable *e, gpointer u) {
    char *s = g_strdup_printf("Echo: %s", gtk_editable_get_text(e));
    (void)u;
    gtk_label_set_text(GTK_LABEL(echo_label), s);
    g_free(s);
}

static void on_activate(GtkApplication *app, gpointer u) {
    (void)u;
    GtkWidget *win = gtk_application_window_new(app);
    gtk_window_set_title(GTK_WINDOW(win), "ReproFixture");
    GtkWidget *box = gtk_box_new(GTK_ORIENTATION_VERTICAL, 8);

    counter_label = gtk_label_new("Count: 0");
    gtk_box_append(GTK_BOX(box), counter_label);

    GtkWidget *inc = gtk_button_new_with_label("Increment");
    g_signal_connect(inc, "clicked", G_CALLBACK(on_inc), NULL);
    gtk_box_append(GTK_BOX(box), inc);

    GtkWidget *entry = gtk_entry_new();
    gtk_accessible_update_property(GTK_ACCESSIBLE(entry),
        GTK_ACCESSIBLE_PROPERTY_LABEL, "msg", -1);
    g_signal_connect(entry, "changed", G_CALLBACK(on_changed), NULL);
    gtk_box_append(GTK_BOX(box), entry);

    echo_label = gtk_label_new("Echo:");
    gtk_box_append(GTK_BOX(box), echo_label);

    gtk_window_set_child(GTK_WINDOW(win), box);
    gtk_window_present(GTK_WINDOW(win));
}

int main(int argc, char **argv) {
    GtkApplication *app = gtk_application_new("dev.reproit.scenariofixture",
        G_APPLICATION_NON_UNIQUE);
    g_signal_connect(app, "activate", G_CALLBACK(on_activate), NULL);
    int status = g_application_run(G_APPLICATION(app), argc, argv);
    g_object_unref(app);
    return status;
}
EOF

# The stub conductor: the modes/barrier.rs wire protocol (the same stub
# pattern runners/rn/scenario.test.mjs validated), recording serve/ack order
# so the harness asserts strict global interleaving from the wire.
# GET /observed returns {"served": [...], "acked": [...]}.
cat > "$WORK/conductor.py" <<'EOF'
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import urlparse, parse_qs

SCRIPT = []
N = 0
state = {"cursor": 0, "served": False, "claimed": 0}
joined = []
observed = {"served": [], "acked": []}


class Handler(BaseHTTPRequestHandler):
    def _reply(self, body):
        data = body.encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, fmt, *args):
        sys.stderr.write("conductor: " + (fmt % args) + "\n")

    def do_GET(self):
        url = urlparse(self.path)
        q = parse_qs(url.query)
        dev = (q.get("device") or [""])[0]
        idx = ord(dev) - 97 if dev else -1
        if url.path == "/claim":
            if state["claimed"] < N:
                role = chr(97 + state["claimed"])
                joined[state["claimed"]] = True
                state["claimed"] += 1
                return self._reply(role)
            return self._reply("ERR full")
        if url.path == "/observed":
            return self._reply(json.dumps(observed))
        if url.path == "/next" and 0 <= idx < N:
            joined[idx] = True
            if state["cursor"] >= len(SCRIPT):
                return self._reply("DONE")
            want_idx, action = SCRIPT[state["cursor"]]
            if not all(joined) or want_idx != idx:
                return self._reply("WAIT")
            if not state["served"]:
                state["served"] = True
                observed["served"].append("%s:%s" % (dev, action))
            return self._reply("ACT\t" + action)
        return self._reply("ERR bad-request")

    def do_POST(self):
        url = urlparse(self.path)
        q = parse_qs(url.query)
        dev = (q.get("device") or [""])[0]
        idx = ord(dev) - 97 if dev else -1
        if url.path == "/done" and 0 <= idx < N:
            if (state["cursor"] < len(SCRIPT)
                    and SCRIPT[state["cursor"]][0] == idx and state["served"]):
                observed["acked"].append("%s:%s" % (dev, SCRIPT[state["cursor"]][1]))
                state["cursor"] += 1
                state["served"] = False
            return self._reply("OK")
        return self._reply("ERR bad-request")


def main():
    global SCRIPT, N, joined
    with open(sys.argv[1], "r", encoding="utf-8") as f:
        raw = json.load(f)
    SCRIPT = [(ord(role) - 97, action) for role, action in raw]
    N = max(i for i, _ in SCRIPT) + 1
    joined = [False] * N
    port = int(sys.argv[2])
    srv = HTTPServer(("127.0.0.1", port), Handler)
    sys.stderr.write("conductor: listening on %d, %d steps, %d actors\n"
                     % (port, len(SCRIPT), N))
    srv.serve_forever()


if __name__ == "__main__":
    main()
EOF

# The interleaved two-actor script: a taps twice, b taps once (isolation), b
# types through EditableText and asserts the echoed label, both assert their
# own counter, a asserts b's text never reached a's instance.
cat > "$WORK/script.json" <<'EOF'
[
  ["a", "tap:Increment"],
  ["b", "tap:Increment"],
  ["a", "tap:Increment"],
  ["b", "type:msg=hello from b"],
  ["b", "assert:text=Echo: hello from b"],
  ["a", "assert:text=Count: 2"],
  ["b", "assert:text=Count: 1"],
  ["a", "assert:count:hello from b=0"]
]
EOF

cat > "$WORK/assert.py" <<'EOF'
import json
import re
import sys

observed_path, a_path, b_path, script_path = sys.argv[1:5]
observed = json.load(open(observed_path))
a_log = open(a_path, encoding="utf-8").read()
b_log = open(b_path, encoding="utf-8").read()
script = json.load(open(script_path))
both = a_log + b_log

failures = []


def check(name, ok, detail=""):
    print("%s %s%s" % ("PASS" if ok else "FAIL", name,
                       (" :: " + detail) if (detail and not ok) else ""))
    if not ok:
        failures.append(name)


expected = ["%s:%s" % (role, action) for role, action in script]

# 1. Strict interleaving observed on the wire: every step served AND acked in
#    the exact global script order.
check("wire: served order == script", observed["served"] == expected,
      "served=%r" % (observed["served"],))
check("wire: acked order == script", observed["acked"] == expected,
      "acked=%r" % (observed["acked"],))

# 2. Distinct roles: one actor claimed a via /claim, one kept env role b.
check("roles: a claimed", "JOURNEY claimed role=a" in a_log)
check("roles: b env-pinned", "JOURNEY claimed role=b" in b_log)

# 3. Each actor executed only its own actions, attributed to its role.
check("attribution: a taps", len(re.findall(r"FUZZ:ACT a tap:Increment", a_log)) == 2)
check("attribution: b tap", len(re.findall(r"FUZZ:ACT b tap:Increment", b_log)) == 1)
check("attribution: b types", "FUZZ:ACT b type:msg=hello from b" in b_log)
check("attribution: no cross-talk",
      "FUZZ:ACT b" not in a_log and "FUZZ:ACT a" not in b_log)

# 4. The EditableText write landed and ran the app's changed handler.
check("type: echo asserted on b",
      'FUZZ:ASSERT pass text="Echo: hello from b" actor=b' in b_log)

# 5. Per-instance isolation: a's counter is 2, b's is 1, and b's typed text
#    never appeared on a's instance.
check("isolation: a count=2", 'FUZZ:ASSERT pass text="Count: 2" actor=a' in a_log)
check("isolation: b count=1", 'FUZZ:ASSERT pass text="Count: 1" actor=b' in b_log)
check("isolation: b text absent on a",
      "FUZZ:ASSERT pass count hello from b want=0 got=0 actor=a" in a_log)

# 6. No misses, no assert failures, no crash blocks anywhere.
check("clean: no FUZZ:MISS", "FUZZ:MISS" not in both)
check("clean: no assert failures", "FUZZ:ASSERT fail" not in both)
check("clean: no crash marker", "EXCEPTION CAUGHT BY REPROIT" not in both)

# 7. Both actors reached DONE and finished green.
check("done: a", "JOURNEY DONE" in a_log and "All tests passed" in a_log)
check("done: b", "JOURNEY DONE" in b_log and "All tests passed" in b_log)

# 8. Both actors observed real states (the scenario observe path emitted a
#    structural signature at least once).
check("observe: a states", "EXPLORE:STATE" in a_log)
check("observe: b states", "EXPLORE:STATE" in b_log)

if failures:
    print("atspi-scenario-e2e: %d assertion(s) failed" % len(failures))
    sys.exit(1)
print("atspi-scenario-e2e: all assertions passed")
EOF

# Runs INSIDE the container, inside dbus-run-session (a fresh session bus).
cat > "$WORK/inner.sh" <<'EOF'
set -euo pipefail
cd /work

# shellcheck disable=SC2046 # pkg-config output is intentionally word-split
gcc $(pkg-config --cflags gtk4) fixture.c $(pkg-config --libs gtk4) -o fixture

# Force GTK4's AT-SPI a11y backend (headless sessions may otherwise pick none).
export GTK_A11Y=atspi
export NO_AT_BRIDGE=0

PORT=8765
python3 conductor.py script.json "$PORT" 2> conductor.err &
CONDUCTOR_PID=$!
for _ in $(seq 1 50); do
  observed_url="http://127.0.0.1:$PORT/observed"
  if python3 -c \
    "import urllib.request;urllib.request.urlopen('$observed_url',timeout=1)" \
    2>/dev/null; then
    break
  fi
  sleep 0.2
done

export REPROIT_TARGET=/work/fixture
export REPROIT_SCENARIO_BARRIER="http://127.0.0.1:$PORT"

# Build the production backend in Linux so this gate works from macOS, Linux,
# and either host CPU architecture; a host-built binary is not portable into
# this container.
cargo build -p reproit --manifest-path /repo/Cargo.toml --target-dir /tmp/reproit-target

# One actor env-pinned to role b (env wins over /claim), one claiming role a.
RUNNER="/tmp/reproit-target/debug/reproit __atspi"
REPROIT_DEVICE=b timeout 180 $RUNNER > b.log 2> b.err &
B_PID=$!
REPROIT_DEVICE= timeout 180 $RUNNER > a.log 2> a.err &
A_PID=$!

A_RC=0; wait "$A_PID" || A_RC=$?
B_RC=0; wait "$B_PID" || B_RC=$?

python3 -c \
  "import urllib.request;print(urllib.request.urlopen('$observed_url',timeout=5).read().decode())" \
  > observed.json
kill "$CONDUCTOR_PID" 2>/dev/null || true

echo "=== actor a (claimed) rc=$A_RC ==="; cat a.log
echo "=== actor b (env role) rc=$B_RC ==="; cat b.log
echo "=== observed ==="; cat observed.json

python3 assert.py observed.json a.log b.log script.json
EOF

# Container entrypoint: virtual display, then a fresh session bus (the a11y
# bus is dbus-activated by the first AT-SPI client), then the harness.
cat > "$WORK/entry.sh" <<'EOF'
set -uo pipefail
Xvfb :99 -screen 0 1280x800x24 > /dev/null 2>&1 &
export DISPLAY=:99
export XDG_RUNTIME_DIR=/tmp/xdg
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
for _ in $(seq 1 50); do
  [ -e /tmp/.X11-unix/X99 ] && break
  sleep 0.1
done
exec dbus-run-session -- bash /work/inner.sh
EOF

docker build -t "$IMAGE" "$WORK"
docker run --rm \
  -v "$ROOT":/repo:ro \
  -v "$WORK":/work \
  "$IMAGE" bash /work/entry.sh
