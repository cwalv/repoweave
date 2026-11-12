#!/usr/bin/env bash
# Builds the oldest git release rwv runs on, and prints the directory holding
# its `git`, for ci-local.sh's git-floor stage.
#
# The release is MINIMUM_RELEASE in src/git.rs, read from there rather than
# written here; `--print-release` prints it and builds nothing.
#
# The build is cached under $RWV_GIT_FLOOR_CACHE (default
# ${XDG_CACHE_HOME:-$HOME/.cache}/rwv/git-floor), outside the repository, one
# directory per release, so it runs once per machine. It downloads the release
# tarball from kernel.org, checks it against the checksum list published there,
# and needs a C compiler, make and zlib's headers. Nothing outside the cache
# directory is written; the git it builds is never installed on PATH.
#
# Built without the parts rwv's suite does not drive: HTTP transport, Perl,
# Python, Tcl/Tk and message translations. A test that switches git to another
# language therefore runs in English against this build, and says so.

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"

floor_release() {
    local release
    release="$(sed -n 's/^const MINIMUM_RELEASE: (u32, u32) = (\([0-9][0-9]*\), \([0-9][0-9]*\));$/\1.\2.0/p' "$repo_root/src/git.rs")"
    if [ -z "$release" ] || [ "$(printf '%s\n' "$release" | wc -l)" -ne 1 ]; then
        printf 'git-floor: src/git.rs declares no single `const MINIMUM_RELEASE: (u32, u32) = (<major>, <minor>);`\n' >&2
        exit 1
    fi
    printf '%s\n' "$release"
}

release="$(floor_release)"
if [ "${1:-}" = "--print-release" ]; then
    printf '%s\n' "$release"
    exit 0
fi
if [ "$#" -ne 0 ]; then
    printf 'usage: %s [--print-release]\n' "$0" >&2
    exit 1
fi

cache="${RWV_GIT_FLOOR_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/rwv/git-floor}"
prefix="$cache/$release"
if [ -x "$prefix/bin/git" ] && [ "$("$prefix/bin/git" version)" = "git version $release" ]; then
    printf '%s\n' "$prefix/bin"
    exit 0
fi

for tool in curl make cc sha256sum tar xz; do
    command -v "$tool" >/dev/null 2>&1 || {
        printf 'git-floor: building git %s needs `%s`, which is not on PATH\n' "$release" "$tool" >&2
        exit 1
    }
done

mirror="https://mirrors.edge.kernel.org/pub/software/scm/git"
tarball="git-$release.tar.xz"
work="$(mktemp -d "${TMPDIR:-/tmp}/rwv-git-floor.XXXXXX")"
trap 'rm -rf "$work"' EXIT

printf 'git-floor: building git %s into %s\n' "$release" "$prefix" >&2
curl -fsSL -o "$work/$tarball" "$mirror/$tarball"
curl -fsSL -o "$work/sha256sums.asc" "$mirror/sha256sums.asc"
grep -E "^[0-9a-f]{64}  $tarball\$" "$work/sha256sums.asc" > "$work/expected" || {
    printf 'git-floor: %s/sha256sums.asc lists no checksum for %s\n' "$mirror" "$tarball" >&2
    exit 1
}
(cd "$work" && sha256sum --check --quiet expected)

mkdir "$work/src"
tar -xJf "$work/$tarball" -C "$work/src" --strip-components=1
rm -rf "$prefix"
# gnu11: compilers that default to C23 reject the older releases' sources.
make -C "$work/src" -j"$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)" \
    prefix="$prefix" CFLAGS="-O2 -std=gnu11" \
    NO_CURL=1 NO_OPENSSL=1 NO_PERL=1 NO_PYTHON=1 NO_TCLTK=1 NO_GETTEXT=1 NO_EXPAT=1 \
    install >"$work/build.log" 2>&1 || {
    tail -n 40 "$work/build.log" >&2
    printf 'git-floor: building git %s failed\n' "$release" >&2
    exit 1
}
[ "$("$prefix/bin/git" version)" = "git version $release" ] || {
    printf 'git-floor: the git built into %s does not report release %s\n' "$prefix" "$release" >&2
    exit 1
}
printf '%s\n' "$prefix/bin"
