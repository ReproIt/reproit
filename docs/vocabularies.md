# Two vocabularies, kept on purpose

`plan-simplification.md` problem 5 asked whether the two redaction forms and
the two divergence markers should converge or whether the split should be
written down permanently. This is the decision: **both splits stay, because both
follow a real boundary, and each is now pinned by the shared behavior vectors
(`sdk/capture-behavior-v1.json`, group `vocabularies`) so neither can drift.**

Leaving it undecided was the only unacceptable outcome.

## Redaction: the split is a type boundary

| where | placeholder | why it must be this |
| --- | --- | --- |
| header value | the string `<reproit:secret>` | a header value is a string. In Rust the map is `BTreeMap<String, String>`; in TypeScript it is `Record<string, string>`. An object cannot live there. |
| body value | the object `{"$reproit": {"redacted": true, "type": ..., "length": ...}}` | a body value is typed JSON, so the placeholder can carry the type and length the replay matcher wildcards on |

Converging would mean one of two losses, both real:

- force headers to carry a JSON encoded placeholder string, so the matcher has
  to parse a string to discover it is a placeholder, for a field that replay
  deliberately does not match on anyway (see the comment on
  `httpRequestMatcher`: recorded headers carry per run noise such as dates and
  connection management, and matching them would turn every replay into a
  divergence)
- force body values to become bare strings, losing the type and length that
  make structure preserving redaction work at all, which is the property that
  lets a scrubbed capture still replay

So this is not two ways of saying one thing. It is one idea expressed correctly
in two type systems.

## Divergence: the split is two consumers

| marker | consumer | contract |
| --- | --- | --- |
| `CAPSULE:MISS` | the fuzz harness | frozen, consumed byte for byte |
| `REPROIT:DIVERGENCE ` + JSON on stderr | the CLI's verdict path | structured, must start the line |

Mobile emits **both**, never one instead of the other. That was added because
the CLI's verdict path parses only the structured marker, so a mobile capsule
replayed through `reproit check` could never have reported `Diverged` while the
runner contract could not be changed without breaking the harness.

Converging here would break the fuzz harness, which is the definition of a
change that costs more than it saves.

## What was missing, and is not any more

The invariant ledger recorded that nothing asserted the two divergence markers
are emitted **together**. All three mobile SDKs did the right thing, and a
platform silently dropping the structured marker would have gone unnoticed
while misreporting a mobile divergence, which is the exact defect the dual
emission existed to fix.

`sdk/capture-behavior-v1.json` now carries a `vocabularies` group, and the React
Native suite asserts both markers are present and that the frozen contract is
still the thrown error. Proven by negative control: removing the structured
marker while keeping `CAPSULE:MISS` fails the suite.

## If you ever revisit this

The redaction split becomes convergeable only if headers stop being a string
map, which would be a protocol change, not a cleanup. The divergence split
becomes convergeable only if the fuzz harness stops consuming `CAPSULE:MISS`
byte for byte. Neither is on the roadmap, so the correct action today is to
leave both and keep them pinned.
