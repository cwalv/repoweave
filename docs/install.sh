#!/bin/sh
# install.sh — download and install the repoweave binaries: rwv and the
# plugins bundled with it (rwv-each), side by side.
#
# Usage:
#   curl -fsSL https://cwalv.github.io/repoweave/install.sh | sh
#
# Installs to ~/.local/bin by default. Set INSTALL_DIR to override:
#   curl -fsSL https://cwalv.github.io/repoweave/install.sh | INSTALL_DIR=/usr/local/bin sh
#
# Set VERSION to pin a specific release (default: latest):
#   curl -fsSL https://cwalv.github.io/repoweave/install.sh | VERSION=0.25.0 sh
# A release before 0.25.0 ships no rwv-each; install one of those with the
# installer published beside its own documentation:
#   curl -fsSL https://cwalv.github.io/repoweave/v0.24.0/install.sh | VERSION=0.24.0 sh
#
# A file already at INSTALL_DIR/rwv or INSTALL_DIR/rwv-each is replaced only
# when it is a repoweave binary. Anything else, such as your own rwv-each
# script, stops the install before anything is written.

set -eu

REPO="cwalv/repoweave"
BINARIES="rwv rwv-each"
FIRST_VERSION_WITH_EVERY_BINARY="0.25.0"
REPOWEAVE_MARKER="cwalv/repoweave"
SITE_URL="https://cwalv.github.io/repoweave"
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

# Succeeds when MAJOR.MINOR.PATCH version $1 is at least $2. A pre-release or
# build suffix on $1 is ignored.
version_at_least() {
    _have="${1%%[-+]*}"
    _want="$2"
    for _i in 1 2 3; do
        _h="${_have%%.*}"
        _w="${_want%%.*}"
        case "$_h" in
            ''|*[!0-9]*) err "VERSION must be MAJOR.MINOR.PATCH, such as 0.25.0; got '$1'" ;;
        esac
        _have="${_have#"$_h"}"; _have="${_have#.}"
        _want="${_want#"$_w"}"; _want="${_want#.}"
        if [ "$_h" -gt "$_w" ]; then return 0; fi
        if [ "$_h" -lt "$_w" ]; then return 1; fi
    done
    return 0
}

# --- existing files ---------------------------------------------------------

# Never executes the file: it may be anyone's script. A release binary is never
# a #! script, and every rwv and rwv-each release binary holds the marker.
is_repoweave_binary() {
    [ -f "$1" ] && [ ! -L "$1" ] || return 1
    [ "$(dd if="$1" bs=2 count=1 2>/dev/null)" != "#!" ] || return 1
    LC_ALL=C grep -q -F "$REPOWEAVE_MARKER" "$1"
}

refuse_foreign_files() {
    for _bin in $BINARIES; do
        _dest="${INSTALL_DIR}/${_bin}"
        if [ -e "$_dest" ] || [ -L "$_dest" ]; then
            if ! is_repoweave_binary "$_dest"; then
                case "$_bin" in
                    rwv-*) _fix="If it is your own \`rwv ${_bin#rwv-}\` plugin, rename it to another verb (for example ${INSTALL_DIR}/rwv-my${_bin#rwv-}, run as \`rwv my${_bin#rwv-}\`) or move it out of ${INSTALL_DIR}, then re-run." ;;
                    *)     _fix="Move it out of ${INSTALL_DIR}, or set INSTALL_DIR to another directory, then re-run." ;;
                esac
                err "${_dest} exists and is not a repoweave binary; nothing was installed. ${_fix}"
            fi
        fi
    done
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

archived() {
    if [ -f "${_extracted_dir}/$1" ]; then
        echo "${_extracted_dir}/$1"
    elif [ -f "${_tmpdir}/$1" ]; then
        echo "${_tmpdir}/$1"
    fi
}

# --- main -------------------------------------------------------------------

main() {
    need_cmd uname
    need_cmd tar
    need_cmd mkdir
    need_cmd dd
    need_cmd grep

    _os="$(detect_os)"
    _arch="$(detect_arch)"
    _target="$(target_triple "$_os" "$_arch")"
    _version="$(resolve_version)"
    _tag="v${_version}"

    if ! version_at_least "$_version" "$FIRST_VERSION_WITH_EVERY_BINARY"; then
        err "repoweave ${_version} predates ${FIRST_VERSION_WITH_EVERY_BINARY}, the first release that ships every one of: ${BINARIES}; nothing was installed. Install it with its own installer: curl -fsSL ${SITE_URL}/v${_version}/install.sh | VERSION=${_version} sh"
    fi
    refuse_foreign_files

    _archive="repoweave-${_target}.tar.xz"
    _url="${BASE_URL}/download/${_tag}/${_archive}"

    printf 'Installing repoweave %s (%s)\n' "$_version" "$_target"
    info "from  ${_url}"
    info "to    ${INSTALL_DIR}"

    _tmpdir="$(mktemp -d)"
    trap 'rm -rf "$_tmpdir"' EXIT

    info "downloading..."
    download "$_url" "${_tmpdir}/${_archive}"

    # GNU tar unpacks .tar.xz through an xz on PATH; other tars need none.
    case "$(tar --version 2>/dev/null)" in
        *"GNU tar"*) need_cmd xz ;;
    esac
    info "extracting..."
    tar xf "${_tmpdir}/${_archive}" -C "$_tmpdir"

    mkdir -p "$INSTALL_DIR"

    # cargo-dist places the binaries inside a directory named after the archive stem.
    _extracted_dir="${_tmpdir}/repoweave-${_target}"
    for _bin in $BINARIES; do
        [ -n "$(archived "$_bin")" ] || err "could not find ${_bin} in archive; nothing was installed"
    done

    _uninstall=""
    for _bin in $BINARIES; do
        install -m 755 "$(archived "$_bin")" "${INSTALL_DIR}/${_bin}"
        info "installed ${_bin} to ${INSTALL_DIR}/${_bin}"
        _uninstall="${_uninstall} ${INSTALL_DIR}/${_bin}"
    done

    printf '\n'
    printf 'To uninstall: rm%s\n\n' "$_uninstall"

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
