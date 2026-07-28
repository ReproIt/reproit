# nnn issue 2120 field review

The report says commit `353c1a7` broke filtering so typed filters left the full list
visible. The affected revision is the parent of the maintainer-named fix, and
`c73600a` reverts the faulty filter change.

Each run used a new xterm-compatible PTY containing `alpha.txt`, `beta.txt`, and
`gamma.txt`. After `/al`, all three affected runs retained the nonmatching terminal
rows. All three fixed runs emitted the row-clear sequences for `beta.txt` and
`gamma.txt`. Filtering by `a` retained all three legitimate matches at both revisions.

The minimized trigger is typing `/al` in the three-file fixture. Review confirmed
`terminal:filter:nonmatching-rows-retained` as the reported bug. Process memory was not
sampled by this bounded PTY harness and is recorded as unavailable.
