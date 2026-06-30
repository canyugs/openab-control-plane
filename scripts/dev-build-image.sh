#!/usr/bin/env bash
# Build a local OCP image for Docker Desktop Kubernetes development.
set -euo pipefail

IMAGE_NAME="${IMAGE_NAME:-openab-control-plane}"
PLATFORM="${PLATFORM:-}"
TAG="${TAG:-}"
TAG_LOCAL=1

usage() {
  cat <<'USAGE'
Usage:
  scripts/dev-build-image.sh [--tag <tag>] [--platform <platform>] [--no-local-tag]

Options:
  --tag <tag>             Image tag to build. Default: dev-<git-sha>-<timestamp>.
  --platform <platform>   Docker platform. Default: host arch mapped to linux/*.
  --no-local-tag          Do not also tag the image as openab-control-plane:local.

Environment:
  IMAGE_NAME              Default: openab-control-plane.
  TAG                     Same as --tag.
  PLATFORM                Same as --platform.

Examples:
  scripts/dev-build-image.sh
  scripts/dev-build-image.sh --tag local --platform linux/arm64
USAGE
}

die() {
  echo "error: $*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag) TAG="${2:?--tag needs a value}"; shift 2 ;;
    --tag=*) TAG="${1#*=}"; shift ;;
    --platform) PLATFORM="${2:?--platform needs a value}"; shift 2 ;;
    --platform=*) PLATFORM="${1#*=}"; shift ;;
    --no-local-tag) TAG_LOCAL=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

need docker
need git

if [[ -z "$PLATFORM" ]]; then
  case "$(uname -m)" in
    arm64|aarch64) PLATFORM="linux/arm64" ;;
    x86_64|amd64) PLATFORM="linux/amd64" ;;
    *) die "unknown host arch; pass --platform explicitly" ;;
  esac
fi

if [[ -z "$TAG" ]]; then
  sha=$(git rev-parse --short HEAD 2>/dev/null || echo "nogit")
  TAG="dev-${sha}-$(date +%Y%m%d%H%M%S)"
fi

tags=(-t "${IMAGE_NAME}:${TAG}")
if [[ "$TAG_LOCAL" == "1" && "$TAG" != "local" ]]; then
  tags+=(-t "${IMAGE_NAME}:local")
fi

echo "building ${IMAGE_NAME}:${TAG} for ${PLATFORM}"
docker buildx build --platform "$PLATFORM" --load "${tags[@]}" .

echo
echo "built image: ${IMAGE_NAME}:${TAG}"
if [[ "$TAG_LOCAL" == "1" ]]; then
  echo "updated alias: ${IMAGE_NAME}:local"
fi
echo "next: scripts/dev-deploy-k8s.sh --image ${IMAGE_NAME}:${TAG}"
