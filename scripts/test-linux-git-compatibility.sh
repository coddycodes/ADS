#!/usr/bin/env bash
set -euo pipefail

# Build the real server integration test on the oldest supported glibc baseline
# before passing it here. Container packages never change the host installation.
if [[ $# -lt 1 ]]; then
  printf 'Usage: %s TEST_BINARY [IMAGE ...]\n' "$0" >&2
  exit 2
fi

binary=$(realpath "$1")
shift
if [[ ! -f "$binary" || ! -x "$binary" ]]; then
  printf 'Integration test binary is not executable: %s\n' "$binary" >&2
  exit 2
fi
engine=${CONTAINER_ENGINE:-docker}
command -v "$engine" >/dev/null
command -v timeout >/dev/null

images=("$@")
if [[ ${#images[@]} -eq 0 ]]; then
  images=(
    docker.io/library/debian:12-slim
    docker.io/library/debian:13-slim
    docker.io/library/ubuntu:22.04
    docker.io/library/ubuntu:24.04
    registry.fedoraproject.org/fedora:44
    docker.io/library/archlinux:base
  )
fi

container=""
cleanup() {
  if [[ -n "$container" ]]; then
    "$engine" rm --force "$container" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

failed=0
index=0
for image in "${images[@]}"; do
  case "$image" in
    docker.io/library/debian:*|docker.io/library/ubuntu:*)
      install='apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends gcc libc6-dev git ca-certificates'
      ;;
    registry.fedoraproject.org/fedora:*)
      install='dnf install -y gcc git ca-certificates'
      ;;
    docker.io/library/archlinux:base)
      install='pacman -Syu --noconfirm --needed gcc git ca-certificates'
      ;;
    *)
      printf 'Unsupported test image: %s\n' "$image" >&2
      exit 2
      ;;
  esac

  index=$((index + 1))
  container="coddy-studio-git-compat-$$-$index"
  printf '\nTesting Git/AppImage isolation on %s\n' "$image"
  if timeout --kill-after=15s 15m "$engine" run --rm \
    --name "$container" \
    --volume "$binary:/coddy-studio-git-test:ro,z" \
    "$image" sh -eu -c "$install
      cat /etc/os-release
      git --version
      /coddy-studio-git-test --list | grep -Fx 'git_subprocesses_ignore_appimage_libraries: test'
      /coddy-studio-git-test --exact git_subprocesses_ignore_appimage_libraries --nocapture"; then
    printf 'PASS %s\n' "$image"
  else
    status=$?
    printf 'FAIL %s (exit %s)\n' "$image" "$status" >&2
    failed=1
  fi
  cleanup
  container=""
done

exit "$failed"
