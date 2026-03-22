#!/usr/bin/env bash

set -euo pipefail

readonly ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly CARGO_TOML="${ROOT_DIR}/Cargo.toml"
readonly PUBLISH_PACKAGES=(
  wisp-core
  wisp-config
  wisp-fuzzy
  wisp-tmux
  wisp-zoxide
  wisp-status
  wisp-ui
  wisp-preview
  wisp-app
  wisp
)
readonly VALIDATION_STEPS=(
  "cargo fmt --check"
  "cargo clippy --workspace --all-targets --all-features -- -D warnings"
  "cargo test --workspace --all-targets"
  "cargo test -p wisp --test smoke"
  "cargo bench -p wisp-core --bench projections --no-run"
  "cargo bench -p wisp-status --bench formatting --no-run"
)

usage() {
  cat <<'EOF'
Usage:
  scripts/release.sh prepare <version>
  scripts/release.sh verify-tag [tag]
  scripts/release.sh publish [--dry-run] [tag]

Commands:
  prepare     Bump the workspace version, refresh Cargo.lock, and run release validation.
  verify-tag  Fail unless the provided tag (or the current exact tag) matches Cargo.toml.
  publish     Verify the tag, run release validation, preflight package all crates, and publish in dependency order.
EOF
}

die() {
  echo "error: $*" >&2
  exit 1
}

run_cmd() {
  echo "+ $*"
  (cd "${ROOT_DIR}" && "$@")
}

current_version() {
  python3 - "${CARGO_TOML}" <<'PY'
import pathlib
import re
import sys

text = pathlib.Path(sys.argv[1]).read_text()
match = re.search(r'(?ms)^\[workspace\.package\]\n.*?^version = "([^"]+)"$', text)
if not match:
    raise SystemExit("workspace.package.version not found")
print(match.group(1))
PY
}

validate_version() {
  [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "version must match X.Y.Z"
}

require_clean_tree() {
  local status
  status="$(cd "${ROOT_DIR}" && git status --porcelain)"
  [[ -z "${status}" ]] || die "git tree must be clean before preparing a release"
}

bump_workspace_version() {
  local version="$1"

  python3 - "${CARGO_TOML}" "${version}" <<'PY'
import pathlib
import re
import sys

path = pathlib.Path(sys.argv[1])
version = sys.argv[2]
text = path.read_text()
updated, count = re.subn(
    r'(?ms)^(\[workspace\.package\]\n.*?^version = ")([^"]+)(")$',
    rf'\g<1>{version}\3',
    text,
    count=1,
)
if count != 1:
    raise SystemExit("failed to update workspace.package.version")
path.write_text(updated)
PY
}

run_validation() {
  local step
  for step in "${VALIDATION_STEPS[@]}"; do
    echo "+ ${step}"
    (cd "${ROOT_DIR}" && eval "${step}")
  done
}

refresh_lockfile() {
  echo "+ cargo metadata --format-version 1 >/dev/null"
  (cd "${ROOT_DIR}" && cargo metadata --format-version 1 >/dev/null)
}

tag_to_version() {
  local tag="$1"
  [[ "${tag}" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]] || die "tag must match vX.Y.Z"
  echo "${BASH_REMATCH[1]}"
}

resolve_tag() {
  if [[ $# -gt 0 && -n "${1}" ]]; then
    echo "$1"
    return
  fi

  local tag
  tag="$(cd "${ROOT_DIR}" && git describe --exact-match --tags HEAD 2>/dev/null || true)"
  [[ -n "${tag}" ]] || die "no tag supplied and HEAD is not at an exact tag"
  echo "${tag}"
}

wait_for_crate_version() {
  local package="$1"
  local version="$2"
  local attempt
  local body

  for attempt in $(seq 1 24); do
    body="$(curl --silent --show-error --location "https://crates.io/api/v1/crates/${package}" || true)"
    if [[ -n "${body}" ]] && grep -q "\"num\":\"${version}\"" <<<"${body}"; then
      return 0
    fi

    echo "waiting for ${package} ${version} to appear on crates.io (attempt ${attempt}/24)"
    sleep 10
  done

  die "timed out waiting for ${package} ${version} to appear on crates.io"
}

publish_packages() {
  local version="$1"
  local package
  [[ -n "${CARGO_REGISTRY_TOKEN:-}" ]] || die "CARGO_REGISTRY_TOKEN must be set for publishing"

  for package in "${PUBLISH_PACKAGES[@]}"; do
    echo "publishing ${package} ${version}"
    if ! run_cmd cargo publish --package "${package}" --locked; then
      cat >&2 <<EOF
publish failed for ${package} ${version}

If this was a partial release rerun, inspect crates.io to confirm which packages already landed
before retrying. Do not retag a different commit with the same version.
EOF
      exit 1
    fi

    if [[ "${package}" != "wisp" ]]; then
      wait_for_crate_version "${package}" "${version}"
    fi
  done
}

preflight_packages() {
  local version="$1"
  local package

  for package in "${PUBLISH_PACKAGES[@]}"; do
    echo "preflighting ${package} ${version}"
    run_cmd cargo package --package "${package}" --locked --no-verify
  done
}

cmd_prepare() {
  [[ $# -eq 1 ]] || die "prepare requires exactly one version argument"

  local version="$1"
  local existing_version
  validate_version "${version}"
  require_clean_tree

  existing_version="$(current_version)"
  [[ "${version}" != "${existing_version}" ]] || die "version ${version} is already current"

  bump_workspace_version "${version}"
  refresh_lockfile
  run_validation
}

cmd_verify_tag() {
  [[ $# -le 1 ]] || die "verify-tag accepts at most one tag argument"

  local tag version manifest_version
  tag="$(resolve_tag "${1:-}")"
  version="$(tag_to_version "${tag}")"
  manifest_version="$(current_version)"

  [[ "${version}" == "${manifest_version}" ]] || die "tag ${tag} does not match workspace version ${manifest_version}"
  echo "verified ${tag} matches workspace version ${manifest_version}"
}

cmd_publish() {
  local dry_run="false"
  local tag=""

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --dry-run)
        dry_run="true"
        ;;
      -*)
        die "unknown option: $1"
        ;;
      *)
        [[ -z "${tag}" ]] || die "publish accepts at most one tag argument"
        tag="$1"
        ;;
    esac
    shift
  done

  tag="$(resolve_tag "${tag}")"
  cmd_verify_tag "${tag}"
  require_clean_tree
  run_validation
  preflight_packages "$(tag_to_version "${tag}")"
  if [[ "${dry_run}" == "false" ]]; then
    publish_packages "$(tag_to_version "${tag}")"
  fi
}

main() {
  [[ $# -gt 0 ]] || {
    usage
    exit 1
  }

  local command="$1"
  shift

  case "${command}" in
    prepare)
      cmd_prepare "$@"
      ;;
    verify-tag)
      cmd_verify_tag "$@"
      ;;
    publish)
      cmd_publish "$@"
      ;;
    -h|--help|help)
      usage
      ;;
    *)
      die "unknown command: ${command}"
      ;;
  esac
}

main "$@"
