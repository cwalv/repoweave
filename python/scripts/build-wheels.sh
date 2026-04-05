#!/usr/bin/env bash
#
# Downloads cargo-dist release artifacts from GitHub and builds platform-specific
# Python wheels that bundle the rwv binary using python/build_wheels.py.
#
# Usage (run from the repository root):
#
#   python/scripts/build-wheels.sh <version>
#   e.g. python/scripts/build-wheels.sh 0.2.0
#
# Produces wheels in python/repoweave/dist/ ready for `twine upload dist/*`.
# Requires: curl, tar/unzip, python3 (stdlib only — no extra packages needed).

set -euo pipefail

VERSION="${1:?Usage: build-wheels.sh <version>}"
TAG="v${VERSION}"
BASE_URL="https://github.com/cwalv/repoweave/releases/download/${TAG}"

# Map: rust-target:python-platform-tag:archive-extension:binary-name
TARGETS=(
  "aarch64-apple-darwin:macosx_11_0_arm64:tar.xz:rwv"
  "x86_64-apple-darwin:macosx_10_12_x86_64:tar.xz:rwv"
  "aarch64-unknown-linux-gnu:manylinux_2_17_aarch64.manylinux2014_aarch64:tar.xz:rwv"
  "x86_64-unknown-linux-gnu:manylinux_2_17_x86_64.manylinux2014_x86_64:tar.xz:rwv"
  "x86_64-pc-windows-msvc:win_amd64:zip:rwv.exe"
)

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BUILD_SCRIPT="${REPO_ROOT}/python/build_wheels.py"
DIST_DIR="${REPO_ROOT}/python/repoweave/dist"
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

mkdir -p "$DIST_DIR"

for entry in "${TARGETS[@]}"; do
  IFS=':' read -r rust_target plat_tag ext bin_name <<< "$entry"

  artifact="repoweave-${rust_target}.${ext}"
  url="${BASE_URL}/${artifact}"
  artifact_path="${WORK_DIR}/${artifact}"

  echo "==> Downloading ${artifact}..."
  curl -fsSL -o "$artifact_path" "$url"

  # Extract binary
  bin_dir="${WORK_DIR}/bin-${rust_target}"
  mkdir -p "$bin_dir"

  if [[ "$ext" == "zip" ]]; then
    unzip -o -j "$artifact_path" "${bin_name}" -d "$bin_dir/"
  else
    tar -xJf "$artifact_path" -C "${WORK_DIR}"
    archive_dir="repoweave-${rust_target}"
    cp "${WORK_DIR}/${archive_dir}/${bin_name}" "${bin_dir}/${bin_name}"
    chmod +x "${bin_dir}/${bin_name}"
  fi

  echo "==> Building wheel for ${plat_tag}..."
  python3 "$BUILD_SCRIPT" \
    --version "$VERSION" \
    --binary "${bin_dir}/${bin_name}" \
    --platform "$plat_tag" \
    --out "$DIST_DIR"
done

# Also build the sdist (requires hatchling / build frontend).
if python3 -m build --version >/dev/null 2>&1; then
  echo "==> Building sdist..."
  (cd "${REPO_ROOT}/python/repoweave" && python3 -m build --sdist --outdir "$DIST_DIR/")
else
  echo "==> Skipping sdist (pip install build to enable)"
fi

echo ""
echo "All distributions built in ${DIST_DIR}/"
ls -la "$DIST_DIR/"
