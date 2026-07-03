#!/usr/bin/env bash

set -euo pipefail

readonly ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly CARGO_TOML="${ROOT_DIR}/Cargo.toml"
# wisp-embers is excluded from the workspace, so it can't inherit
# version.workspace and must be bumped explicitly during a release.
readonly EMBERS_CARGO_TOML="${ROOT_DIR}/crates/wisp-embers/Cargo.toml"
# Publish order is a topological sort of the internal dependency graph: every
# crate appears after the wisp-* crates it depends on, so each `cargo publish`
# resolves its dependencies against versions already live on crates.io.
# wisp-embers is excluded from the workspace but is an (optional) dependency of
# wisp-app and wisp, and wisp-kindra is a leaf dependency of wisp — both must be
# published or the crates that depend on them cannot be.
readonly PUBLISH_PACKAGES=(
  wisp-core
  wisp-config
  wisp-fuzzy
  wisp-kindra
  wisp-tmux
  wisp-zoxide
  wisp-embers
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
  prepare     Bump workspace and internal dependency versions, refresh Cargo.lock, and run release validation.
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

# Bump the `version = "..."` field inside a TOML section header (e.g.
# `workspace.package` or `package`), asserting the expected number of matches so
# a manifest layout change fails the release instead of silently no-op'ing.
bump_section_version() {
  local file="$1"
  local section="$2"
  local version="$3"
  local expected_matches="$4"

  python3 - "${file}" "${section}" "${version}" "${expected_matches}" <<'PY'
import pathlib
import re
import sys

path = pathlib.Path(sys.argv[1])
section = sys.argv[2]
version = sys.argv[3]
expected = int(sys.argv[4])
text = path.read_text()
# Match the version field only within the target section: after the header,
# consume lines lazily but never cross into the next `[section]` header, so a
# section without a version fails here instead of bumping a later section's.
pattern = (
    r'(?m)^(\[' + re.escape(section) + r'\]\n'
    r'(?:(?!\[)[^\n]*\n)*?'
    r'version = ")([^"]+)(")$'
)
updated, count = re.subn(pattern, rf'\g<1>{version}\3', text, count=1)
if count != expected:
    raise SystemExit(
        f"failed to update [{section}].version in {path} "
        f"(matched {count}, expected {expected})"
    )
path.write_text(updated)
PY
}

bump_workspace_versions() {
  local version="$1"

  bump_section_version "${CARGO_TOML}" "workspace.package" "${version}" 1

  # Bump the internal path-dependency requirements (unique to the workspace
  # manifest, so this stays outside the shared section-version helper).
  python3 - "${CARGO_TOML}" "${version}" <<'PY'
import pathlib
import re
import sys

path = pathlib.Path(sys.argv[1])
version = sys.argv[2]
text = path.read_text()
# Independently count every internal wisp path dependency, then require the
# strict version-bump regex to match all of them. This fails the release on a
# reformatted or missed dependency instead of silently leaving an old version.
expected = len(re.findall(r'(?m)^wisp(?:-[a-z]+)? = \{.*path = "crates/', text))
updated, count = re.subn(
    r'(?m)^((?:wisp(?:-[a-z]+)?) = \{ version = ")([^"]+)(", path = "crates/[^"]+" \})$',
    rf'\g<1>{version}\3',
    text,
)
if count != expected:
    raise SystemExit(
        f"failed to update internal workspace dependency versions "
        f"(matched {count}, expected {expected})"
    )
path.write_text(updated)
PY

  # The wisp-embers requirement above is bumped to the new version, but the
  # excluded crate's own [package] version is not part of the workspace, so bump
  # it here in lockstep or `cargo metadata` fails to resolve the path dependency.
  bump_section_version "${EMBERS_CARGO_TOML}" "package" "${version}" 1
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
  # Refresh the excluded crate's own lockfile too so its committed Cargo.lock
  # reflects the bumped wisp-embers version.
  echo "+ cargo metadata --manifest-path ${EMBERS_CARGO_TOML} --format-version 1 >/dev/null"
  (cd "${ROOT_DIR}" && cargo metadata --manifest-path "${EMBERS_CARGO_TOML}" --format-version 1 >/dev/null)
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

  local -a target_args
  for package in "${PUBLISH_PACKAGES[@]}"; do
    echo "publishing ${package} ${version}"
    # wisp-embers lives outside the workspace, so it is addressed by its own
    # manifest path rather than `--package` from the workspace root.
    if [[ "${package}" == "wisp-embers" ]]; then
      target_args=(--manifest-path "${EMBERS_CARGO_TOML}")
    else
      target_args=(--package "${package}")
    fi
    if ! run_cmd cargo publish "${target_args[@]}" --locked; then
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

  # Package every workspace member in one invocation so internal path
  # dependencies resolve against the local crates being released rather than
  # crates.io. Packaging a crate individually (the previous approach) fails for
  # any crate that depends on a bumped-but-not-yet-published sibling, because
  # `cargo package` resolves each dependency's version requirement against the
  # registry, where the new version does not exist yet.
  #
  # wisp-app and the wisp binary are excluded here: their optional wisp-embers
  # dependency lives outside the workspace, so cargo cannot substitute a local
  # copy and the requirement can't resolve until wisp-embers is published. They
  # are validated during the ordered publish instead, once wisp-embers and their
  # other dependencies are live on crates.io.
  echo "preflighting workspace crates ${version}"
  run_cmd cargo package --workspace --exclude wisp-app --exclude wisp --locked --no-verify

  # wisp-embers is excluded from the workspace, so preflight it via its own
  # manifest.
  echo "preflighting wisp-embers ${version}"
  run_cmd cargo package --manifest-path "${EMBERS_CARGO_TOML}" --locked --no-verify
}

cmd_prepare() {
  [[ $# -eq 1 ]] || die "prepare requires exactly one version argument"

  local version="$1"
  local existing_version
  validate_version "${version}"
  require_clean_tree

  existing_version="$(current_version)"
  [[ "${version}" != "${existing_version}" ]] || die "version ${version} is already current"

  bump_workspace_versions "${version}"
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
