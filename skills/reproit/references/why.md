# Fault localization (`reproit repro why`)

```sh
reproit repro why <id>  # rank suspect files for a finding
reproit repro why       # rank for the current failing suite
```

`why` uses **Ochiai** spectrum-based fault localization: it compares which code ran during failing
repros vs passing ones. Files that run on the failing path but rarely on passing paths score
highest.

## How to use the ranking

1. Open the **top-ranked file first**. The score is a suspicion ranking, not a guarantee, but the
   top few are where to look.
2. Cross-reference with the repro's action sequence and (for crashes) the stack trace. The
   intersection of "ran on failure" + "on the stack" is usually the bug.
3. A flat ranking (everything scored similarly) means the failure touches shared/common code. Lean
   on the stack trace instead.

## Caveats

- Needs both failing and passing runs to discriminate; a brand-new repro with no passing siblings
  gives weaker signal.
- It localizes _executed_ code. A bug of omission (missing guard, missing case) shows up as the file
  that _should_ have handled the input, look at callers of the top-ranked file too.
