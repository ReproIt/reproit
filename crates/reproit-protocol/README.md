# reproit-protocol

`reproit-protocol` is the IO-free contract shared by Reproit capture SDKs, the
CLI, and Reproit Cloud. It defines bounded, versioned evidence, occurrence,
reproduction, and diagnostic receipt types.

Consumers must call each type's validation method after deserialization. New
wire meanings require an explicitly dispatched wire version. The package
version follows semantic versioning independently of the CLI and Cloud release
versions.

The machine-readable wire-version ledger is in `contract.json`. Reproit tests
that ledger against the compiled constants so release automation cannot publish
a package whose advertised contract differs from its implementation.
