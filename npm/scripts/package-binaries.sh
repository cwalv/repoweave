#!/usr/bin/env bash
#
# Downloads cargo-dist release artifacts and places the binaries into
# the correct platform-specific npm packages.
#
# Usage: ./package-binaries.sh <version>
#   e.g. ./package-binaries.sh 0.1.0
#
# Expects to be run from the npm/ directory.

set -euo pipefail

VERSION="${1:?Usage: package-binaries.sh <version>}"
TAG="v${VERSION}"
BASE_URL="https://github.com/cwalv/repoweave/releases/download/${TAG}"

# Map: npm-package-dir  artifact-name  binary-name-in-archive
declare -A PACKAGES=(
  ["repoweave-darwin-arm64"]="repoweave-aarch64-apple-darwin.tar.xz"
  ["repoweave-darwin-x64"]="repoweave-x86_64-apple-darwin.tar.xz"
  ["repoweave-linux-arm64"]="repoweave-aarch64-unknown-linux-gnu.tar.xz"
  ["repoweave-linux-x64"]="repoweave-x86_64-unknown-linux-gnu.tar.xz"
  ["repoweave-windows-x64"]="repoweave-x86_64-pc-windows-msvc.zip"
)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
NPM_DIR="$(dirname "$SCRIPT_DIR")"
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

for pkg_dir in "${!PACKAGES[@]}"; do
  artifact="${PACKAGES[$pkg_dir]}"
  url="${BASE_URL}/${artifact}"

  echo "==> Downloading ${artifact}..."
  curl -fsSL -o "${WORK_DIR}/${artifact}" "${url}"

  mkdir -p "${NPM_DIR}/${pkg_dir}/bin"

  echo "==> Extracting binary for ${pkg_dir}..."
  if [[ "$artifact" == *.zip ]]; then
    unzip -o -j "${WORK_DIR}/${artifact}" "*/rwv.exe" -d "${NPM_DIR}/${pkg_dir}/bin/"
  else
    # tar.xz — extract the rwv binary from the archive
    tar -xJf "${WORK_DIR}/${artifact}" -C "${WORK_DIR}"
    # cargo-dist archives have the structure: repoweave-<target>/rwv
    archive_dir="${artifact%.tar.xz}"
    cp "${WORK_DIR}/${archive_dir}/rwv" "${NPM_DIR}/${pkg_dir}/bin/rwv"
    chmod +x "${NPM_DIR}/${pkg_dir}/bin/rwv"
  fi

  echo "==> Done: ${pkg_dir}"
done

echo ""
echo "All platform binaries packaged. Ready to publish."
