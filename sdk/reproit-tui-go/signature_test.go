package reproittui

// Canonical TUI screen-signature PARITY test for the reproit-tui Go SDK.
//
// This is the TUI mirror of the Rust backend's own tests
// (crates/reproit/src/backends/tui.rs) and of the a11y parity gates
// (sdk/test/signature_test.js, runners/test_signature.py). It LOADS
// tui_signature_vectors.json and, for every vector, asserts that the SDK's
// StructuralSig(contents, cursor) and ContentFingerprint(contents, cursor) equal
// the values the REAL tui.rs code produced for the same screen (see the JSON's
// _derivation note). That proves the SDK's screen descriptor + hashing is byte-for-
// byte identical to the runner's, in the TUI namespace (NOT signature_vectors.json,
// which is the a11y Node-tree namespace).
//
// Run: `go test ./...` from sdk/reproit-tui.
//
// No em dashes anywhere, per project rules.

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

type goldenVector struct {
	Name        string    `json:"name"`
	Contents    string    `json:"contents"`
	Cursor      [2]uint16 `json:"cursor"`
	ExpectedSig string    `json:"expected_sig"`
	ExpectedFP  string    `json:"expected_fp"`
}

type goldenFile struct {
	Vectors []goldenVector `json:"vectors"`
}

func loadGolden(t *testing.T) []goldenVector {
	t.Helper()
	// The TUI golden vectors live at the repo root (shared by the Rust/Go/TS/Python
	// TUI SDKs). go test runs with CWD = this package dir, two levels below root.
	path := filepath.Join("..", "..", "tui_signature_vectors.json")
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("reading %s: %v", path, err)
	}
	var gf goldenFile
	if err := json.Unmarshal(raw, &gf); err != nil {
		t.Fatalf("parsing %s: %v", path, err)
	}
	if len(gf.Vectors) < 18 {
		t.Fatalf("need >= 18 golden TUI vectors, got %d", len(gf.Vectors))
	}
	return gf.Vectors
}

// TestGoldenVectorsMatchTuiRs is THE parity gate: every golden vector's signature
// and fingerprint must equal what tui.rs produced for the same screen.
func TestGoldenVectorsMatchTuiRs(t *testing.T) {
	for _, v := range loadGolden(t) {
		gotSig := StructuralSig(v.Contents, v.Cursor[0], v.Cursor[1])
		if gotSig != v.ExpectedSig {
			t.Errorf("%s: structural_sig mismatch: skeleton=%q got %s want %s",
				v.Name, skeletonOf(v.Contents), gotSig, v.ExpectedSig)
		}
		gotFP := ContentFingerprint(v.Contents, v.Cursor[0], v.Cursor[1])
		if gotFP != v.ExpectedFP {
			t.Errorf("%s: content_fingerprint mismatch: got %s want %s",
				v.Name, gotFP, v.ExpectedFP)
		}
	}
}

// sigByName fetches a golden expected_sig (and fp) by case name for the relationship
// assertions below.
func sigByName(t *testing.T, vs []goldenVector, name string) (string, string) {
	t.Helper()
	for _, v := range vs {
		if v.Name == name {
			return v.ExpectedSig, v.ExpectedFP
		}
	}
	t.Fatalf("no golden vector named %q", name)
	return "", ""
}

// TestCrossVectorRelationships pins the spec-promised facts about the TUI signature,
// the same ones tui.rs's own tests assert (locale-invariance, value-class buckets,
// the value-sensitive fingerprint, cursor-as-structure). These hold over the golden
// expected values themselves, so they double-check the JSON is internally consistent
// with the contract, then re-check that the SDK reproduces those same values.
func TestCrossVectorRelationships(t *testing.T) {
	vs := loadGolden(t)

	en, enFP := sigByName(t, vs, "login_en")
	de, deFP := sigByName(t, vs, "login_de")
	if en != de {
		t.Errorf("locale-invariance: login_en (%s) must equal login_de (%s)", en, de)
	}
	if enFP == deFP {
		t.Errorf("the content fingerprint must still differ across locales (words differ)")
	}

	c0, _ := sigByName(t, vs, "count0")
	c1, _ := sigByName(t, vs, "count1")
	c12, _ := sigByName(t, vs, "count12")
	c3, _ := sigByName(t, vs, "count3")
	c7, _ := sigByName(t, vs, "count7")
	if c0 == c1 || c1 == c12 || c0 == c12 {
		t.Errorf("value-class: 0(ZERO)/1(POS1)/12(POS2) must be three distinct sigs: %s %s %s", c0, c1, c12)
	}
	if c1 != c3 || c1 != c7 {
		t.Errorf("value-class: 1, 3, 7 all POS1 must collapse: %s %s %s", c1, c3, c7)
	}

	h100, h100FP := sigByName(t, vs, "hits100")
	h101, h101FP := sigByName(t, vs, "hits101")
	if h100 != h101 {
		t.Errorf("100 and 101 share skeleton + POS3 bucket -> same sig, got %s %s", h100, h101)
	}
	if h100FP == h101FP {
		t.Errorf("content fingerprint must move on a value-only change: %s %s", h100FP, h101FP)
	}

	enCur3, _ := sigByName(t, vs, "login_en_cur3")
	if en == enCur3 {
		t.Errorf("cursor cell is structural: focusing a different field must change the sig")
	}

	// And the SDK must REPRODUCE every one of those expected values (not just the
	// JSON being self-consistent): re-derive from the contents and compare.
	for _, v := range vs {
		if got := StructuralSig(v.Contents, v.Cursor[0], v.Cursor[1]); got != v.ExpectedSig {
			t.Errorf("%s: SDK does not reproduce expected sig (%s vs %s)", v.Name, got, v.ExpectedSig)
		}
	}
}

// TestSigOfCanonicalFnv1aFamily pins the FNV-1a primitive to the canonical known
// values, matching tui.rs::tests::sig_of_is_the_canonical_fnv1a_family and the
// oracle's fnv1a32_hex. This catches any drift in the hash primitive itself.
func TestSigOfCanonicalFnv1aFamily(t *testing.T) {
	if got := SigOf(""); got != "811c9dc5" {
		t.Errorf(`SigOf("") = %s, want 811c9dc5 (FNV-1a 32 offset basis)`, got)
	}
	if got := SigOf("a"); got != "e40c292c" {
		t.Errorf(`SigOf("a") = %s, want e40c292c`, got)
	}
}

// TestValueClassMatchesOracleBuckets pins the value_class bucketer + strict-decimal
// grammar, matching tui.rs::value_class / is_strict_decimal and the oracle's buckets.
func TestValueClassMatchesOracleBuckets(t *testing.T) {
	cases := []struct {
		in, want string
	}{
		{"", "EMPTY"}, {"   ", "EMPTY"},
		{"0", "ZERO"}, {"0.0", "ZERO"}, {"-0", "ZERO"},
		{"-3", "NEG"}, {"-0.5", "NEG"},
		{"3", "POS1"}, {"9.99", "POS1"}, {"+7", "POS1"},
		{"10", "POS2"}, {"99", "POS2"}, {"  42  ", "POS2"},
		{"100", "POS3"}, {"999.99", "POS3"},
		{"1000", "POSL"}, {"123456", "POSL"},
		{"1,234", "NONEMPTY"}, {"1.234.567", "NONEMPTY"}, {"1 234", "NONEMPTY"},
		{"$5", "NONEMPTY"}, {"5%", "NONEMPTY"}, {"1e3", "NONEMPTY"}, {"0x10", "NONEMPTY"},
		{".", "NONEMPTY"}, {"3.", "NONEMPTY"}, {".5", "NONEMPTY"}, {"--5", "NONEMPTY"},
		{"hello", "NONEMPTY"},
	}
	for _, c := range cases {
		if got := valueClass(c.in); got != c.want {
			t.Errorf("valueClass(%q) = %s, want %s", c.in, got, c.want)
		}
	}
}

// TestNumericValueClassesBounded pins the bounded, sorted multiset behavior, matching
// tui.rs::numeric_value_classes (cap at maxValueClasses, sorted deterministically).
func TestNumericValueClassesBounded(t *testing.T) {
	got := numericValueClasses("a 7 b 0 c 50")
	want := []string{"POS1", "POS2", "ZERO"}
	if len(got) != len(want) {
		t.Fatalf("numericValueClasses sorted multiset: got %v want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("numericValueClasses sorted multiset: got %v want %v", got, want)
		}
	}
	// many numbers -> capped at maxValueClasses
	many := ""
	for n := 0; n < 50; n++ {
		many += itoa(n) + " "
	}
	if g := numericValueClasses(many); len(g) != maxValueClasses {
		t.Errorf("numeric value-class set must be capped at %d, got %d", maxValueClasses, len(g))
	}
	if g := numericValueClasses("no numbers here"); len(g) != 0 {
		t.Errorf("no numeric tokens -> empty set, got %v", g)
	}
}

func itoa(n int) string {
	if n == 0 {
		return "0"
	}
	var b []byte
	for n > 0 {
		b = append([]byte{byte('0' + n%10)}, b...)
		n /= 10
	}
	return string(b)
}

// TestScreenContentsTextMatchesVt100 pins the cell-grid -> contents-string renderer
// (ScreenContents.Text) against the vt100 screen().contents() model: per-row and
// trailing-blank-row whitespace trimmed, gaps before non-empty cells become spaces,
// wide cells skip their spacer column.
func TestScreenContentsTextMatchesVt100(t *testing.T) {
	// "Count: 0" then a blank row: trailing blank row is trimmed, no trailing spaces.
	sc := ScreenContents{
		Grid: [][]Cell{
			cells("Count: 0   "), // trailing spaces must be trimmed
			cells("           "), // fully blank row -> trimmed entirely
		},
		CursorRow: 0, CursorCol: 8,
	}
	if got, want := sc.Text(), "Count: 0"; got != want {
		t.Errorf("Text() = %q, want %q", got, want)
	}
	// And the signature off the grid equals the golden count0 signature.
	if got := StructuralSig(sc.Text()+"\n", sc.CursorRow, sc.CursorCol); got != "d80c050b" {
		// note: count0 golden contents is "Count: 0\n"; vt100 trims trailing newlines so
		// a single-row screen's contents has NO trailing \n. The golden vector used a
		// literal "Count: 0\n" string fed straight to structural_sig (the runner's unit
		// tests do the same), so we re-derive against that exact string here.
		t.Errorf("grid-derived sig (with trailing newline) = %s, want d80c050b", got)
	}

	// A gap before a later non-empty cell becomes a single space per empty column.
	sc2 := ScreenContents{Grid: [][]Cell{{{Contents: "a"}, {}, {}, {Contents: "b"}}}}
	if got, want := sc2.Text(), "a  b"; got != want {
		t.Errorf("gap rendering: Text() = %q, want %q", got, want)
	}

	// A wide cell emits its contents once and the spacer column is skipped.
	sc3 := ScreenContents{Grid: [][]Cell{{{Contents: "欢", Wide: true}, {}, {Contents: "x"}}}}
	if got, want := sc3.Text(), "欢x"; got != want {
		t.Errorf("wide-cell rendering: Text() = %q, want %q", got, want)
	}

	// Raw passthrough.
	sc4 := ScreenContents{Raw: "verbatim\nrows"}
	if got := sc4.Text(); got != "verbatim\nrows" {
		t.Errorf("Raw passthrough: got %q", got)
	}
}

func cells(s string) []Cell {
	rs := []rune(s)
	out := make([]Cell, len(rs))
	for i, r := range rs {
		if r == ' ' {
			out[i] = Cell{} // a space in the helper means an empty cell
		} else {
			out[i] = Cell{Contents: string(r)}
		}
	}
	return out
}
