#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
BUILD_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/reproit-backend-build.XXXXXX")"

native_platform() {
  case "$(docker info --format '{{.Architecture}}')" in
    arm64|aarch64) printf 'linux/arm64\n' ;;
    amd64|x86_64) printf 'linux/amd64\n' ;;
    *)
      echo "unsupported Docker architecture" >&2
      return 1
      ;;
  esac
}

PLATFORM="${REPROIT_FIELD_PLATFORM:-$(native_platform)}"

cleanup() {
  find "$BUILD_ROOT" -depth -delete
}
trap cleanup EXIT INT TERM

fetch_revision() {
  local repository="$1" revision="$2" destination="$3"
  git init -q "$destination"
  git -C "$destination" remote add origin "$repository"
  git -C "$destination" fetch -q --depth=1 origin "$revision"
  git -C "$destination" checkout -q --detach FETCH_HEAD
  test "$(git -C "$destination" rev-parse HEAD)" = "$revision"
}

build_image() {
  local repository="$1" revision="$2" image="$3" dockerfile="$4" source="$5"
  fetch_revision "$repository" "$revision" "$source"
  docker build \
    --platform "$PLATFORM" \
    --file "$ROOT/validation/field/backend_contract/$dockerfile" \
    --label "org.opencontainers.image.revision=$revision" \
    --label "org.opencontainers.image.source=$repository" \
    --tag "$image" \
    "$source"
  test "$(docker image inspect "$image" \
    --format '{{index .Config.Labels "org.opencontainers.image.revision"}}')" = "$revision"
}

build_image \
  https://github.com/go-gitea/gitea.git \
  98c61942aa433342eacf08e4040ded80b1d0efe1 \
  reproit-field-gitea:98c61942 Dockerfile.gitea "$BUILD_ROOT/gitea-affected"
build_image \
  https://github.com/go-gitea/gitea.git \
  4812e354866a066dcb899af667b0fad5fa094065 \
  reproit-field-gitea:4812e354 Dockerfile.gitea "$BUILD_ROOT/gitea-fixed"
build_image \
  https://github.com/usememos/memos.git \
  14fb38f37560541bf2719647e7e8b1468937f8ef \
  reproit-field-memos:14fb38f3 Dockerfile.memos "$BUILD_ROOT/memos-affected"
build_image \
  https://github.com/usememos/memos.git \
  7c3fcc297d8e5a955d9c0bc4f3ca917854132e8e \
  reproit-field-memos:7c3fcc29 Dockerfile.memos "$BUILD_ROOT/memos-fixed"

cmp \
  "$BUILD_ROOT/gitea-affected/templates/swagger/v1_json.tmpl" \
  "$BUILD_ROOT/gitea-fixed/templates/swagger/v1_json.tmpl"
diff -qr \
  "$BUILD_ROOT/memos-affected/proto/api/v1" \
  "$BUILD_ROOT/memos-fixed/proto/api/v1"
cmp \
  "$BUILD_ROOT/memos-affected/proto/gen/openapi.yaml" \
  "$BUILD_ROOT/memos-fixed/proto/gen/openapi.yaml"

docker image inspect \
  reproit-field-gitea:98c61942 \
  reproit-field-gitea:4812e354 \
  reproit-field-memos:14fb38f3 \
  reproit-field-memos:7c3fcc29 \
  --format '{{.RepoTags}} {{.Id}} {{.Architecture}} {{json .Config.Labels}}'
