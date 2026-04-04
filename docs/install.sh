#!/bin/sh
# install.sh — download and install the repoweave (rwv) binary.
#
# Usage:
#   curl -fsSL https://cwalv.github.io/repoweave/install.sh | sh
#
# Installs to ~/.local/bin by default. Set INSTALL_DIR to override:
#   curl -fsSL https://cwalv.github.io/repoweave/install.sh | INSTALL_DIR=/usr/local/bin sh
#
# Set VERSION to pin a specific release (default: latest):
#   curl -fsSL https://cwalv.github.io/repoweave/install.sh | VERSION=0.1.0 sh

set -eu

REPO="cwalv/repoweave"
BINARY="rwv"
INSTALL_DIR="${INSTALL_DIR:-"$HOME/.local/bin"}"
BASE_URL="https://github.com/${REPO}/releases"

# --- helpers ----------------------------------------------------------------

info()  { printf '  %s\n' "$@"; }
err()   { printf 'Error: %s\n' "$@" >&2; exit 1; }

need_cmd() {
    if ! command -v "$1" > /dev/null 2>&1; then
        err "need '$1' (command not found)"
    fi
}

# --- detect platform --------------------------------------------------------

detect_os() {
    case "$(uname -s)" in
        Linux*)  echo "linux" ;;
        Darwin*) echo "macos" ;;
        *)       err "unsupported OS: $(uname -s)" ;;
    esac
}

detect_arch() {
    case "$(uname -m)" in
        x86_64|amd64)  echo "x86_64" ;;
        aarch64|arm64) echo "aarch64" ;;
        *)             err "unsupported architecture: $(uname -m)" ;;
    esac
}

# Map OS + arch to cargo-dist target triple.
target_triple() {
    _os="$1"
    _arch="$2"
    case "${_os}" in
        linux) echo "${_arch}-unknown-linux-gnu" ;;
        macos) echo "${_arch}-apple-darwin" ;;
    esac
}

# --- resolve version --------------------------------------------------------

resolve_version() {
    if [ -n "${VERSION:-}" ]; then
        echo "$VERSION"
        return
    fi
    # Follow the "latest" redirect and extract the tag from the final URL.
    _url=$(fetch_redirect "${BASE_URL}/latest")
    echo "$_url" | sed 's|.*/v\{0,1\}||'
}

# --- download helpers -------------------------------------------------------

has_curl() { command -v curl > /dev/null 2>&1; }
has_wget() { command -v wget > /dev/null 2>&1; }

fetch_redirect() {
    if has_curl; then
        curl -fsSL -o /dev/null -w '%{url_effective}' "$1"
    elif has_wget; then
        wget -q --max-redirect=5 --spider --server-response "$1" 2>&1 \
            | grep -i 'Location:' | tail -1 | awk '{print $2}' | tr -d '\r'
    else
        err "need curl or wget"
    fi
}

download() {
    _url="$1"
    _out="$2"
    if has_curl; then
        curl -fsSL -o "$_out" "$_url"
    elif has_wget; then
        wget -q -O "$_out" "$_url"
    else
        err "need curl or wget"
    fi
}

# --- main -------------------------------------------------------------------

main() {
    need_cmd uname
    need_cmd tar
    need_cmd mkdir

    _os="$(detect_os)"
    _arch="$(detect_arch)"
    _target="$(target_triple "$_os" "$_arch")"
    _version="$(resolve_version)"
    _tag="v${_version}"

    _archive="repoweave-${_target}.tar.xz"
    _url="${BASE_URL}/download/${_tag}/${_archive}"

    printf 'Installing repoweave %s (%s)\n' "$_version" "$_target"
    info "from  ${_url}"
    info "to    ${INSTALL_DIR}/${BINARY}"

    _tmpdir="$(mktemp -d)"
    trap 'rm -rf "$_tmpdir"' EXIT

    info "downloading..."
    download "$_url" "${_tmpdir}/${_archive}"

    need_cmd xz
    info "extracting..."
    tar xf "${_tmpdir}/${_archive}" -C "$_tmpdir"

    mkdir -p "$INSTALL_DIR"

    # cargo-dist places the binary inside a directory named after the archive stem.
    _extracted_dir="${_tmpdir}/repoweave-${_target}"
    if [ -f "${_extracted_dir}/${BINARY}" ]; then
        install -m 755 "${_extracted_dir}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
    elif [ -f "${_tmpdir}/${BINARY}" ]; then
        install -m 755 "${_tmpdir}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
    else
        err "could not find ${BINARY} binary in archive"
    fi

    info "installed ${BINARY} to ${INSTALL_DIR}/${BINARY}"
    printf '\n'

    # Check PATH
    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) ;;
        *)
            printf 'NOTE: %s is not in your PATH.\n' "$INSTALL_DIR"
            printf 'Add it by appending one of the following to your shell profile:\n\n'
            printf '  # bash (~/.bashrc)\n'
            printf '  export PATH="%s:$PATH"\n\n' "$INSTALL_DIR"
            printf '  # zsh (~/.zshrc)\n'
            printf '  export PATH="%s:$PATH"\n\n' "$INSTALL_DIR"
            printf '  # fish (~/.config/fish/config.fish)\n'
            printf '  fish_add_path %s\n\n' "$INSTALL_DIR"
            ;;
    esac
}

main "$@"
