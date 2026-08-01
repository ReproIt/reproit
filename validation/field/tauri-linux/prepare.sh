#!/usr/bin/env bash
# Prepare phase. Builds the pinned x86_64 Tauri worker and materializes the
# subject checkout with its locked dependencies. This is the only phase allowed
# to use the network: every later phase runs the application offline.
set -euo pipefail

: "${CAMPAIGN_IMAGE:?}" "${CAMPAIGN_SUBJECT:?}" "${CAMPAIGN_FIELD:?}"
: "${CAMPAIGN_REPOSITORY:?}" "${CAMPAIGN_DEPS_MARKER:?}" "${CAMPAIGN_INSTALL:?}"

docker build --platform linux/amd64 -t "$CAMPAIGN_IMAGE" "$CAMPAIGN_FIELD" >/dev/null

built=""
for _ in $(seq 1 30); do
  if docker image inspect "$CAMPAIGN_IMAGE" >/dev/null 2>&1; then built=yes; break; fi
  sleep 2
done
test -n "$built" || { echo "worker image never became resolvable" >&2; exit 1; }

# Fail closed on the declared bound: tauri-linux claims x86_64 Linux, so a
# natively built arm64 worker is a hard error rather than a silent fallback.
arch="$(docker image inspect --format '{{.Architecture}}' "$CAMPAIGN_IMAGE")"
test "$arch" = "amd64" || { echo "worker image is $arch, not amd64" >&2; exit 1; }

if [ ! -d "$CAMPAIGN_SUBJECT/.git" ]; then
  mkdir -p "$(dirname "$CAMPAIGN_SUBJECT")"
  git clone -q "$CAMPAIGN_REPOSITORY" "$CAMPAIGN_SUBJECT"
fi

if [ ! -d "$CAMPAIGN_SUBJECT/$CAMPAIGN_DEPS_MARKER" ]; then
  docker run --rm --platform linux/amd64 \
    -v "$CAMPAIGN_SUBJECT:/work" -v "$CAMPAIGN_FIELD:/field:ro" \
    "$CAMPAIGN_IMAGE" bash -lc "
      set -eu
      export CI=true
      $CAMPAIGN_INSTALL" >/dev/null
fi

# Subjects that take their library from argv need a deterministic set of books.
# They are generated, not downloaded, so every run sees the same bytes and the
# campaign stays offline.
if [ -n "${CAMPAIGN_BOOKS:-}" ]; then
  mkdir -p "$CAMPAIGN_BOOKS"
  for index in $(seq -w 1 "${CAMPAIGN_BOOK_COUNT:?}"); do
    book="$CAMPAIGN_BOOKS/field-book-$index.txt"
    [ -f "$book" ] && continue
    {
      printf 'Reproit Field Book %s\n\n' "$index"
      for paragraph in $(seq 1 40); do
        printf 'Paragraph %d of field book %s. ' "$paragraph" "$index"
        printf 'This text exists only to give the reader a stable body of prose.\n\n'
      done
    } > "$book"
  done
  echo "books: $(find "$CAMPAIGN_BOOKS" -name '*.txt' | wc -l | tr -d ' ')"
fi

echo "prepared $(git -C "$CAMPAIGN_SUBJECT" rev-parse --short HEAD)"
