# Offline support-bundle security

The `.rpb` format is a bounded evidence carrier. It is not an executable package.

## Guarantees

- Every artifact is identified by the SHA-256 digest of its exact bytes.
- The encrypted payload uses XChaCha20-Poly1305 with a fresh 24-byte nonce.
- The manifest includes the ciphertext digest and is signed with Ed25519.
- Inspection verifies the manifest, payload digest, and signature without decrypting artifacts.
- Import authenticates and decrypts before extracting.
- Archive entries must have exactly the form `artifacts/<64 hexadecimal characters>`.
- Collection accepts at most 128 regular files and 64 MiB of plaintext.
- The header is limited to 1 MiB and the complete imported file is bounded.
- Output files are created without overwrite.
- Generated encryption keys are written separately with mode 0600 on Unix.
- Import and inspection require a signer key supplied independently from the bundle.

## Authenticity boundary

When `REPROIT_BUNDLE_SIGNING_KEY` is configured, the signature authenticates that collector to an
importer that holds its public key. Without a configured key, collection generates an ephemeral
signing key and writes `<bundle>.signer`. Transfer this public key through a different authorized
channel. Import and inspection compare the manifest signer to
`REPROIT_BUNDLE_TRUSTED_SIGNER`, the public key derived from
`REPROIT_BUNDLE_SIGNING_KEY`, or that separate signer file. An embedded key alone is never trusted.

The encryption key comes from `REPROIT_BUNDLE_ENCRYPTION_KEY` as 64 hexadecimal characters. When
the variable is absent, collection generates a random key and writes `<bundle>.key`. Send the key
through a different authorized channel. Possession of the bundle and key permits reading every
included artifact.

## Policy boundary

Encryption does not make evidence exportable. Default collection marks artifacts
`local-analysis-only` and `unredacted-restricted`. `--exportable` is the operator's assertion that
redaction already happened at source and that policy permits export. Reproit does not infer consent
from a filename, file type, encryption setting, or destination.

Imported strings, paths from the source environment, logs, dump contents, and manifest labels are
untrusted data. They never become argv, environment values, working directories, cleanup actions,
or provider definitions. Execution is allowed only after an explicit plan binding resolves to a
checkout-owned provider with an exact matching digest.

## Current limits

The first format encrypts the archive in memory and therefore uses a 64 MiB plaintext limit. Large
dumps need a streaming encryption and resumable transfer format. The format does not wrap a
symmetric key to an asymmetric recipient key or manage organization key rotation.
