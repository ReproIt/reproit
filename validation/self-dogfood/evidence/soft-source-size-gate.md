# Soften the source-size gate: guideline plus hard ceiling

The 1,000-line check was a hard wall, and a mechanical rustfmt pass pushed
sdk/reproit-backend-rs/src/capture.rs from under it to 1,057 lines, turning
main red (run 30729490312, job rust) with no semantic change anywhere.
Founder decision: good engineering is being around the amount, not a strict
count. The check now warns between 1,000 and 1,200 lines, naming each file
and asking for a split with the next real change, and fails only past 1,200.
Verified: the check passes on the current workspace with two named warnings
(capture.rs 1,057; reproit-site styles.css 1,023) and still fails on a
constructed 1,300-line file.
