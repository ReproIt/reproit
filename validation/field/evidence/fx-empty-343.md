# fx issue 343 field review

The report says an empty file leaves fx showing an indexing spinner indefinitely. The
affected revision is the parent of `14b2139`, and that commit sends an explicit EOF
message to the terminal model.

Each run used a new xterm-compatible PTY and a zero-byte file. All three affected runs
rendered `indexing` after EOF. All three fixed runs rendered the completed `0%` status
instead. A non-empty one-object JSON file rendered normally at both revisions.

The minimized trigger is opening one empty file. Review confirmed
`terminal:empty-file:indexing-after-eof` as the reported bug. Process memory was not
sampled by this bounded PTY harness and is recorded as unavailable.
