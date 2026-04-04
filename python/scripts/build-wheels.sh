#!/usr/bin/env bash
#
# Downloads cargo-dist release artifacts and builds platform-specific
# Python wheels that bundle the rwv binary.
#
# Usage: ./build-wheels.sh <version>
#   e.g. ./build-wheels.sh 0.1.0
#
# Produces wheels in dist/ ready for `twine upload`.
# Expects to be run from the python/repoweave/ directory.

set -euo pipefail

VERSION="${1:?Usage: build-wheels.sh <version>}"
TAG="v${VERSION}"
BASE_URL="https://github.com/cwalv/repoweave/releases/download/${TAG}"

# Map: rust-target  python-platform-tag  archive-extension
TARGETS=(
  "aarch64-apple-darwin:macosx_11_0_arm64:tar.xz"
  "x86_64-apple-darwin:macosx_10_12_x86_64:tar.xz"
  "aarch64-unknown-linux-gnu:manylinux_2_17_aarch64.manylinux2014_aarch64:tar.xz"
  "x86_64-unknown-linux-gnu:manylinux_2_17_x86_64.manylinux2014_x86_64:tar.xz"
  "x86_64-pc-windows-msvc:win_amd64:zip"
)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PKG_DIR="$(dirname "$SCRIPT_DIR")/repoweave"
WORK_DIR="$(mktemp -d)"
DIST_DIR="${PKG_DIR}/dist"
trap 'rm -rf "$WORK_DIR"' EXIT

mkdir -p "$DIST_DIR"

for entry in "${TARGETS[@]}"; do
  IFS=':' read -r rust_target plat_tag ext <<< "$entry"

  artifact="repoweave-${rust_target}.${ext}"
  url="${BASE_URL}/${artifact}"

  echo "==> Downloading ${artifact}..."
  curl -fsSL -o "${WORK_DIR}/${artifact}" "${url}"

  # Extract binary
  bin_dir="${WORK_DIR}/bin-${rust_target}"
  mkdir -p "$bin_dir"

  if [[ "$ext" == "zip" ]]; then
    unzip -o -j "${WORK_DIR}/${artifact}" "*/rwv.exe" -d "$bin_dir/"
    bin_name="rwv.exe"
  else
    tar -xJf "${WORK_DIR}/${artifact}" -C "${WORK_DIR}"
    archive_dir="repoweave-${rust_target}"
    cp "${WORK_DIR}/${archive_dir}/rwv" "$bin_dir/rwv"
    chmod +x "$bin_dir/rwv"
    bin_name="rwv"
  fi

  echo "==> Building wheel for ${plat_tag}..."

  # Place binary in the package's data scripts location so pip installs it
  # into the user's PATH.
  data_scripts="${WORK_DIR}/wheel-data-${rust_target}/repoweave-${VERSION}.data/scripts"
  mkdir -p "$data_scripts"
  cp "$bin_dir/$bin_name" "$data_scripts/$bin_name"

  # Build the wheel using hatchling, with the binary available
  # We use a simpler approach: build a pure wheel then repack with binary
  (
    cd "$PKG_DIR"
    python3 -m build --wheel --outdir "${WORK_DIR}/pure-wheel/" 2>/dev/null || true
  )

  # Repack: take the pure wheel content and add the native binary, fix the platform tag
  pure_whl=$(ls "${WORK_DIR}/pure-wheel/"*.whl 2>/dev/null | head -1)
  if [ -z "$pure_whl" ]; then
    echo "Warning: Could not build pure wheel, skipping ${plat_tag}"
    continue
  fi

  repack_dir="${WORK_DIR}/repack-${rust_target}"
  mkdir -p "$repack_dir"
  unzip -q -o "$pure_whl" -d "$repack_dir"

  # Add the binary to the data scripts directory in the wheel
  mkdir -p "${repack_dir}/repoweave-${VERSION}.data/scripts"
  cp "$bin_dir/$bin_name" "${repack_dir}/repoweave-${VERSION}.data/scripts/$bin_name"

  # Fix the WHEEL tag to reflect the platform
  sed -i "s/Tag: py3-none-any/Tag: py3-none-${plat_tag}/" "${repack_dir}/repoweave-${VERSION}.dist-info/WHEEL"

  # Rewrite the RECORD file
  (
    cd "$repack_dir"
    find . -type f ! -name RECORD -printf '%P\n' | sort | while read -r f; do
      hash=$(python3 -c "import hashlib,base64; d=open('$f','rb').read(); print('sha256='+base64.urlsafe_b64encode(hashlib.sha256(d).digest()).rstrip(b'=').decode())")
      size=$(stat -c%s "$f")
      echo "$f,$hash,$size"
    done > "repoweave-${VERSION}.dist-info/RECORD"
    echo "repoweave-${VERSION}.dist-info/RECORD,," >> "repoweave-${VERSION}.dist-info/RECORD"
  )

  # Package as wheel
  wheel_name="repoweave-${VERSION}-py3-none-${plat_tag}.whl"
  (cd "$repack_dir" && zip -q -r "${DIST_DIR}/${wheel_name}" .)

  echo "==> Built: ${wheel_name}"
done

# Also produce a source distribution
(cd "$PKG_DIR" && python3 -m build --sdist --outdir "$DIST_DIR/" 2>/dev/null || true)

echo ""
echo "All wheels built in ${DIST_DIR}/"
ls -la "$DIST_DIR/"
