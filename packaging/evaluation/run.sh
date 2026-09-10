#!/usr/bin/env bash
set -euo pipefail

readonly DEFAULT_IMAGE="ghcr.io/canyugs/ocp-review-eval:0.1.0"
readonly DESTINATION_SOCKET="/var/run/docker.sock"

scratch_dir=""
output_dir=""
repo=""
revision=""
base=""
findings=""
evidence=""
models=""
environment_file=""
auth_env_file=""
docker_socket="$DESTINATION_SOCKET"
docker_gid=""
run_uid="$(id -u)"
run_gid="$(id -g)"
image="$DEFAULT_IMAGE"
auth_env_keys=()
auth_env_values=()

die() {
  printf 'evaluation launcher: %s\n' "$*" >&2
  exit 2
}

parse_auth_env_file() {
  local file="$1"
  local line
  local key
  local value
  local line_number=0
  local existing_key

  while IFS= read -r line || [[ -n "$line" ]]; do
    line_number=$((line_number + 1))
    if [[ "$line" =~ ^[[:space:]]*$ || "$line" =~ ^[[:space:]]*# ]]; then
      continue
    fi
    [[ "$line" == *=* ]] || die "authentication environment file contains a malformed record at line ${line_number}"

    key="${line%%=*}"
    value="${line#*=}"
    [[ -n "$key" ]] || die "authentication environment file contains a malformed record at line ${line_number}"
    case "$key" in
      CLAUDE_CODE_OAUTH_TOKEN|ANTHROPIC_API_KEY|ANTHROPIC_AUTH_TOKEN)
        ;;
      *)
        die "authentication environment file contains unsupported key: ${key}"
        ;;
    esac

    if ((${#auth_env_keys[@]} > 0)); then
      for existing_key in "${auth_env_keys[@]}"; do
        [[ "$existing_key" != "$key" ]] || die "authentication environment file contains a duplicate key: ${key}"
      done
    fi
    auth_env_keys+=("$key")
    auth_env_values+=("$value")
  done < "$file"
}

usage() {
  cat >&2 <<'EOF'
Usage: run.sh --scratch-dir ABS_DIR --output-dir ABS_DIR --repo ABS_DIR
  --revision FULL_SHA --base FULL_SHA --findings ABS_FILE --evidence ABS_DIR
  --models ABS_FILE [--environment ABS_FILE] [--auth-env-file ABS_FILE]
  [--docker-socket ABS_SOCKET] [--docker-gid INTEGER] [--uid INTEGER]
  [--gid INTEGER] [--image IMAGE]

The scratch directory is mounted at the identical absolute path inside the
outer evaluator container and is exported as TMPDIR. The outer container is
given the host Docker socket; generated validation containers receive no
socket mount. The repository is copied into a temporary child of scratch with
symlinks preserved, and only that child's parent is mounted read-only. The
temporary copy is removed after Docker exits. --uid must match the invoking
non-root host UID. Use this only on a dedicated runner.
EOF
}

require_value() {
  (($# >= 2)) || die "missing value for $1"
  [[ -n "$2" ]] || die "empty value for $1"
}

validate_path_text() {
  local value="$1"
  local label="$2"
  [[ "$value" == /* ]] || die "$label must be an absolute path"
  [[ "$value" != "/" ]] || die "$label may not be /"
  case "$value" in
    *,*|*$'\n'*|*$'\r'*) die "$label contains a character unsafe for a literal mount" ;;
  esac
  case "$value" in
    */./*|*/../*|*/.|*/..) die "$label must not contain . or .. path components" ;;
  esac
}

reject_symlink_components() {
  local path="$1"
  local current="$path"
  while [[ "$current" != "/" ]]; do
    [[ ! -L "$current" ]] || die "path contains a symlink component: $path"
    current="${current%/*}"
    [[ -n "$current" ]] || current="/"
  done
}

canonical_existing_path() {
  local path="$1"
  local parent
  local name
  if [[ -d "$path" ]]; then
    (cd -- "$path" && pwd -P)
    return
  fi
  parent="${path%/*}"
  name="${path##*/}"
  [[ -n "$parent" ]] || parent="/"
  (cd -- "$parent" && printf '%s/%s\n' "$(pwd -P)" "$name")
}

ensure_directory() {
  local path="$1"
  local label="$2"
  validate_path_text "$path" "$label"
  if [[ -L "$path" ]]; then
    die "$label may not be a symlink"
  fi
  if [[ -e "$path" ]]; then
    [[ -d "$path" ]] || die "$label must be a directory"
  else
    local parent="${path%/*}"
    [[ -n "$parent" ]] || parent="/"
    [[ -d "$parent" ]] || die "$label parent directory must already exist"
    reject_symlink_components "$parent"
    mkdir -- "$path" || die "could not create $label"
  fi
  reject_symlink_components "$path"
  [[ -d "$path" && -w "$path" ]] || die "$label must be writable"
}

paths_overlap() {
  local left="$1"
  local right="$2"
  [[ "$left" == "$right" || "$left" == "$right/"* || "$right" == "$left/"* ]]
}

reject_storage_overlap() {
  local storage_path="$1"
  local storage_label="$2"
  local input_path="$3"
  local input_label="$4"
  if paths_overlap "$storage_path" "$input_path"; then
    die "$storage_label overlaps $input_label"
  fi
  return 0
}

require_directory() {
  local path="$1"
  local label="$2"
  validate_path_text "$path" "$label"
  reject_symlink_components "$path"
  [[ -d "$path" ]] || die "$label must be an existing directory"
}

require_file() {
  local path="$1"
  local label="$2"
  validate_path_text "$path" "$label"
  reject_symlink_components "$path"
  [[ -f "$path" ]] || die "$label must be an existing regular file"
}

require_nonzero_decimal() {
  local value="$1"
  local label="$2"
  [[ "$value" =~ ^[1-9][0-9]*$ ]] || die "$label must be a non-zero decimal integer"
}

require_nonnegative_decimal() {
  local value="$1"
  local label="$2"
  [[ "$value" =~ ^[0-9]+$ ]] || die "$label must be a non-negative decimal integer"
}

while (($#)); do
  case "$1" in
    --help|-h)
      usage
      exit 0
      ;;
    --scratch-dir|--output-dir|--repo|--revision|--base|--findings|--evidence|--models|--environment|--auth-env-file|--docker-socket|--docker-gid|--uid|--gid|--image)
      require_value "$@"
      case "$1" in
        --scratch-dir) scratch_dir="$2" ;;
        --output-dir) output_dir="$2" ;;
        --repo) repo="$2" ;;
        --revision) revision="$2" ;;
        --base) base="$2" ;;
        --findings) findings="$2" ;;
        --evidence) evidence="$2" ;;
        --models) models="$2" ;;
        --environment) environment_file="$2" ;;
        --auth-env-file) auth_env_file="$2" ;;
        --docker-socket) docker_socket="$2" ;;
        --docker-gid) docker_gid="$2" ;;
        --uid) run_uid="$2" ;;
        --gid) run_gid="$2" ;;
        --image) image="$2" ;;
      esac
      shift 2
      ;;
    *)
      die "unknown option: $1"
      ;;
  esac
done

[[ -n "$scratch_dir" ]] || die "--scratch-dir is required"
[[ -n "$output_dir" ]] || die "--output-dir is required"
[[ -n "$repo" ]] || die "--repo is required"
[[ -n "$revision" ]] || die "--revision is required"
[[ -n "$base" ]] || die "--base is required"
[[ -n "$findings" ]] || die "--findings is required"
[[ -n "$evidence" ]] || die "--evidence is required"
[[ -n "$models" ]] || die "--models is required"
[[ "$revision" =~ ^[0-9a-fA-F]{40}$ ]] || die "--revision must be a full 40-hex SHA"
[[ "$base" =~ ^[0-9a-fA-F]{40}$ ]] || die "--base must be a full 40-hex SHA"
[[ -n "$image" ]] || die "--image must not be empty"
[[ "$image" != -* ]] || die "--image must not begin with -"
case "$image" in
  *,*|*$'\n'*|*$'\r'*) die "--image contains unsafe characters" ;;
esac
host_uid="$(id -u)"
[[ "$host_uid" != "0" ]] || die "launcher requires a non-root invoking UID (id -u is 0)"
require_nonzero_decimal "$run_uid" "--uid"
require_nonnegative_decimal "$run_gid" "--gid"
require_nonzero_decimal "$host_uid" "invoking UID"
[[ "$run_uid" == "$host_uid" ]] || die "--uid must match invoking host UID ${host_uid} (got ${run_uid})"

validate_path_text "$scratch_dir" "scratch directory"
validate_path_text "$output_dir" "output directory"
if [[ -L "$scratch_dir" ]]; then
  die "scratch directory may not be a symlink"
fi
if [[ -L "$output_dir" ]]; then
  die "output directory may not be a symlink"
fi
reject_symlink_components "$scratch_dir"
reject_symlink_components "$output_dir"
if [[ ! -e "$scratch_dir" ]]; then
  scratch_parent="${scratch_dir%/*}"
  [[ -n "$scratch_parent" ]] || scratch_parent="/"
  [[ -d "$scratch_parent" ]] || die "scratch directory parent must already exist"
  reject_symlink_components "$scratch_parent"
fi
if [[ ! -e "$output_dir" ]]; then
  output_parent="${output_dir%/*}"
  [[ -n "$output_parent" ]] || output_parent="/"
  [[ -d "$output_parent" ]] || die "output directory parent must already exist"
  reject_symlink_components "$output_parent"
fi

require_directory "$repo" "repository"
require_directory "$evidence" "evidence directory"
require_file "$findings" "findings file"
require_file "$models" "models file"
if [[ -n "$environment_file" ]]; then
  require_file "$environment_file" "environment file"
fi
if [[ -n "$auth_env_file" ]]; then
  require_file "$auth_env_file" "authentication environment file"
fi

scratch_dir="$(canonical_existing_path "$scratch_dir")"
output_dir="$(canonical_existing_path "$output_dir")"
repo="$(canonical_existing_path "$repo")"
evidence="$(canonical_existing_path "$evidence")"
findings="$(canonical_existing_path "$findings")"
models="$(canonical_existing_path "$models")"
if [[ -n "$environment_file" ]]; then
  environment_file="$(canonical_existing_path "$environment_file")"
fi
if [[ -n "$auth_env_file" ]]; then
  auth_env_file="$(canonical_existing_path "$auth_env_file")"
fi

if paths_overlap "$scratch_dir" "$output_dir"; then
  die "scratch and output directories must not overlap"
fi
reject_storage_overlap "$scratch_dir" "scratch directory" "$repo" "repository"
reject_storage_overlap "$scratch_dir" "scratch directory" "$evidence" "evidence directory"
reject_storage_overlap "$scratch_dir" "scratch directory" "$findings" "findings file"
reject_storage_overlap "$scratch_dir" "scratch directory" "$models" "models file"
if [[ -n "$environment_file" ]]; then
  reject_storage_overlap "$scratch_dir" "scratch directory" "$environment_file" "environment file"
fi
if [[ -n "$auth_env_file" ]]; then
  reject_storage_overlap "$scratch_dir" "scratch directory" "$auth_env_file" "authentication environment file"
fi
reject_storage_overlap "$output_dir" "output directory" "$repo" "repository"
reject_storage_overlap "$output_dir" "output directory" "$evidence" "evidence directory"
reject_storage_overlap "$output_dir" "output directory" "$findings" "findings file"
reject_storage_overlap "$output_dir" "output directory" "$models" "models file"
if [[ -n "$environment_file" ]]; then
  reject_storage_overlap "$output_dir" "output directory" "$environment_file" "environment file"
fi
if [[ -n "$auth_env_file" ]]; then
  reject_storage_overlap "$output_dir" "output directory" "$auth_env_file" "authentication environment file"
fi

if [[ -n "$auth_env_file" ]]; then
  parse_auth_env_file "$auth_env_file"
fi

ensure_directory "$scratch_dir" "scratch directory"
ensure_directory "$output_dir" "output directory"

if [[ -n "$(find "$scratch_dir" -mindepth 1 -maxdepth 1 -print -quit)" ]]; then
  die "scratch directory must be empty and dedicated"
fi

validate_path_text "$docker_socket" "Docker socket"
[[ -S "$docker_socket" ]] || die "Docker socket must be an existing Unix socket"
if [[ -z "$docker_gid" ]]; then
  docker_gid="$(stat -c '%g' -- "$docker_socket" 2>/dev/null || stat -f '%g' "$docker_socket" 2>/dev/null || true)"
fi
require_nonnegative_decimal "$docker_gid" "--docker-gid"
command -v docker >/dev/null 2>&1 || die "docker executable is not on PATH"

staging_parent=""
# shellcheck disable=SC2329 # Invoked indirectly by the EXIT trap below.
cleanup_staging() {
  local status="$?"
  trap - EXIT
  if [[ -n "$staging_parent" ]]; then
    rm -rf -- "$staging_parent" || printf 'evaluation launcher: could not remove staging directory: %s\n' "$staging_parent" >&2
  fi
  exit "$status"
}
trap cleanup_staging EXIT

staging_parent="$(mktemp -d "${scratch_dir}/ocp-review-eval-staging.XXXXXX")" || die "could not create repository staging directory"
staged_repo="${staging_parent}/repo"
cp -R -P "$repo" "$staged_repo" || die "could not copy repository into staging directory"

docker_args=(
  run
  --rm
  --init
  --user "${run_uid}:${run_gid}"
  --group-add "$docker_gid"
  --mount "type=bind,src=${scratch_dir},dst=${scratch_dir}"
  --mount "type=bind,src=${output_dir},dst=${output_dir}"
  --mount "type=bind,src=${staging_parent},dst=${staging_parent},readonly"
  --mount "type=bind,src=${evidence},dst=${evidence},readonly"
  --mount "type=bind,src=${findings},dst=${findings},readonly"
  --mount "type=bind,src=${models},dst=${models},readonly"
  --mount "type=bind,src=${docker_socket},dst=${DESTINATION_SOCKET}"
)
if [[ -n "$environment_file" ]]; then
  docker_args+=(--mount "type=bind,src=${environment_file},dst=${environment_file},readonly")
fi
# Keep these command-line values explicit. In particular, TMPDIR must remain
# the mapped scratch.
docker_args+=(
  --env "TMPDIR=${scratch_dir}"
  --env "HOME=/tmp/ocp-review-eval-home"
  --env "LANG=C.UTF-8"
  --env "LC_ALL=C.UTF-8"
)

auth_env_index=0
while ((auth_env_index < ${#auth_env_keys[@]})); do
  auth_env_key="${auth_env_keys[auth_env_index]}"
  auth_env_value="${auth_env_values[auth_env_index]}"
  export "${auth_env_key}=${auth_env_value}"
  docker_args+=(--env "$auth_env_key")
  auth_env_index=$((auth_env_index + 1))
done

package_args=(
  run
  --repo "$staged_repo"
  --revision "$revision"
  --base "$base"
  --findings "$findings"
  --evidence "$evidence"
  --models "$models"
  --output "$output_dir"
)
if [[ -n "$environment_file" ]]; then
  package_args+=(--environment "$environment_file")
fi

set +e
docker "${docker_args[@]}" "$image" "${package_args[@]}"
docker_status=$?
set -e
exit "$docker_status"
