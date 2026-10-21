#!/usr/bin/env bash
# Runs the test suite as x86_64-pc-windows-gnu binaries under Wine: a local
# lead on Windows-only failures, ahead of a .github/workflows/windows-tests.yml
# run.
#
# Usage: scripts/windows-wine.sh [cargo test target selectors] [-- test filter]
#
# With no selectors it runs every test target, which is what
# `scripts/ci-local.sh --stages=windows-wine` invokes. Selectors pass through
# to cargo (`--test add_remove_test`, `--lib`), and anything after `--` passes
# to every selected test binary. A whole-suite run takes about as long as that
# workflow's job; a run of the binaries a Windows red names takes minutes.
#
# Wine is not Windows, so a red here is a lead to confirm there, and a green
# here is not a green there. Known reds that are this environment's and not
# the code's:
#
#   tests built on tests/common/compile_probe.rs. They hand the cross-built
#   rlib to rustc, and its proc-macro dependencies were built for the Linux
#   host, not for Windows.
#   tests that build this crate with cargo. raw-dylib makes the Windows-gnu
#   toolchain run dlltool, which needs an assembler that toolchain does not
#   ship.
#   tests that measure npm, go or uv, none of which is installed here.
#   tests that rely on case-insensitive paths. The prefix lives on the host's
#   case-sensitive filesystem.
#
# Needs nix, rustup, curl, unzip, jq, perl and the x86_64-pc-windows-gnu
# target. The first run downloads Wine, a mingw-w64 linker, MinGit and a
# Windows-hosted Rust toolchain into <target-dir>/windows-wine, about 5 GB with
# the prefixes.

set -euo pipefail

NIXPKGS=github:NixOS/nixpkgs/f20cd90e28fe0403873f473b29e42662cd86e8c4
MINGIT_TAG=v2.56.0.windows.2
MINGIT_ZIP=MinGit-2.56.0.2-64-bit.zip
MINGIT_SHA256=da35e72aa21c005a5a0d298cfbae110bc1609a815730ea0dde84b01a1b3cd3be
TARGET=x86_64-pc-windows-gnu
jobs="${WINE_JOBS:-4}"

cargo_args=()
test_args=()
while [ "$#" -gt 0 ]; do
    if [ "$1" = "--" ]; then
        shift
        test_args=("$@")
        break
    fi
    cargo_args+=("$1")
    shift
done
[ "${#cargo_args[@]}" -gt 0 ] || cargo_args=(--lib --bins --tests)

if ! rustup target list --installed | grep -qx "$TARGET"; then
    printf 'windows-wine: %s is not installed — rustup target add %s\n' "$TARGET" "$TARGET" >&2
    exit 1
fi
missing=()
for tool in nix curl unzip jq perl sha256sum; do
    command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
done
if [ "${#missing[@]}" -gt 0 ]; then
    printf 'windows-wine: needs %s on PATH\n' "${missing[*]}" >&2
    exit 1
fi

package_dir=$(pwd)
target_dir=$(cargo metadata --format-version 1 --no-deps | jq -r .target_directory)
cache="$target_dir/windows-wine"
mkdir -p "$cache"

nix_out() {
    nix build --no-link --print-out-paths "$NIXPKGS#$1^out"
}
# Wine 11.0 creates a relative symlink whose target climbs with `..` but
# cannot open a path through it, which is the shape of every weave-root link.
# 11.19 can; the pinned nixpkgs carries it as wineWow64Packages.unstable.
wine_bin="$(nix_out wineWow64Packages.unstable)/bin"
mingw_bin="$(nix_out pkgsCross.mingwW64.buildPackages.gcc)/bin"
pthreads_lib="$(nix_out pkgsCross.mingwW64.windows.pthreads)/lib"
export PATH="$wine_bin:$mingw_bin:$PATH" WINEDEBUG=-all

mingit="$cache/mingit"
if [ ! -f "$mingit/cmd/git.exe" ]; then
    curl -fsSL -o "$cache/$MINGIT_ZIP" \
        "https://github.com/git-for-windows/git/releases/download/$MINGIT_TAG/$MINGIT_ZIP"
    printf '%s  %s\n' "$MINGIT_SHA256" "$cache/$MINGIT_ZIP" | sha256sum -c --quiet
    rm -rf "$mingit.partial"
    unzip -q -d "$mingit.partial" "$cache/$MINGIT_ZIP"
    # The msys runtime prints "Cygwin WARNING: Couldn't compute FAST_CWD
    # pointer" to stderr on every start under Wine, with no setting to stop
    # it, and git runs each local-path clone and fetch through that runtime's
    # sh. Emptying the format string silences that one warning.
    dll="$mingit.partial/usr/bin/msys-2.0.dll"
    if ! perl -0777 -pi -e '$n += s/C(ygwin WARNING:\n  Couldn\x27t compute FAST_CWD)/\x00$1/; END { exit($n == 1 ? 0 : 1) }' "$dll"; then
        printf 'windows-wine: the FAST_CWD warning is not in %s\n' "$dll" >&2
        exit 1
    fi
    cp "$mingit.partial/etc/fstab" "$mingit.partial/etc/fstab.dist"
    rm -rf "$mingit"
    mv "$mingit.partial" "$mingit"
fi

channel=$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)
windows_toolchain="$cache/rustup/toolchains/$channel-$TARGET"
if [ ! -f "$windows_toolchain/bin/cargo.exe" ]; then
    RUSTUP_HOME="$cache/rustup" rustup toolchain install "$channel-$TARGET" \
        --profile minimal --force-non-host --no-self-update
fi

# env!("CARGO_MANIFEST_DIR") and env!("CARGO_BIN_EXE_rwv") are host paths
# such as /home/me/repoweave. A Windows process reads one as rooted on the
# current drive, and msys reads it as rooted at its own install, so each gets
# the host directory mounted at that spelling.
roots=()
for dir in "$package_dir" "$target_dir"; do
    top="/$(printf '%s' "$dir" | cut -d/ -f2)"
    [[ " ${roots[*]} " == *" $top "* ]] || roots+=("$top")
done
{
    cat "$mingit/etc/fstab.dist"
    for top in "${roots[@]}"; do
        printf 'Z:%s %s ntfs binary,posix=0,noacl,user 0 0\n' "$top" "$top"
    done
} > "$mingit/etc/fstab"

for ((slot = 0; slot < jobs; slot++)); do
    prefix="$cache/prefix-$slot"
    if [ ! -d "$prefix/drive_c" ]; then
        if [ -d "$cache/prefix-0/drive_c" ]; then
            cp -a "$cache/prefix-0" "$prefix"
        else
            WINEPREFIX="$prefix" wineboot -i >/dev/null 2>&1
            WINEPREFIX="$prefix" wineserver -w
            printf '[user]\n\temail = ci@example.com\n\tname = CI\n' \
                > "$prefix/drive_c/users/$(id -un)/.gitconfig"
        fi
    fi
    for top in "${roots[@]}"; do
        link="$prefix/drive_c/${top#/}"
        [ -L "$link" ] && continue
        if [ -e "$link" ]; then
            printf 'windows-wine: %s already exists in the prefix; cannot mount %s there\n' "$link" "$top" >&2
            exit 1
        fi
        ln -s "$top" "$link"
    done
done

winpath() {
    printf 'Z:%s' "${1//\//\\}"
}

run_dir="$cache/run"
rm -rf "$run_dir"
mkdir -p "$run_dir"

cargo test --no-run --target "$TARGET" \
    --config "target.$TARGET.linker='x86_64-w64-mingw32-gcc'" \
    --config "target.$TARGET.rustflags=['-L','native=$pthreads_lib','-C','debuginfo=0']" \
    --message-format=json-render-diagnostics "${cargo_args[@]}" \
    | jq -r 'select(.reason == "compiler-artifact" and .executable != null and .profile.test)
             | .executable' > "$run_dir/binaries"

run_one() {
    local exe=$1
    shift
    # The host toolchain's variables would reach the Windows process as they
    # are: CARGO_HOME would point the in-Wine cargo at the host's registry,
    # and a weave's rustc-wrapper names a program Wine cannot start. Wine
    # encodes a non-ASCII file name for the host with the host locale, which
    # a Nix-built Wine does not find under the caller's LANG and falls back
    # to ASCII, so creating "café.txt" fails with NotFound.
    env -u CARGO_HOME -u RUSTUP_HOME -u RUSTUP_TOOLCHAIN -u RUST_RECURSION_COUNT -u LC_ALL \
        LANG=C.UTF-8 WINEPREFIX="$WINE_CACHE/prefix-$SLOT" WINEPATH="$WINE_WINDOWS_PATH" \
        CARGO="$WINE_WINDOWS_CARGO" RUSTC_WRAPPER= CARGO_MANIFEST_DIR="$WINE_PACKAGE_DIR" \
        wine "$exe" "$@" > "$WINE_RUN_DIR/$(basename "$exe" .exe).log" 2>&1 \
        || printf '%s\n' "$exe" >> "$WINE_RUN_DIR/failed"
}
export -f run_one
export WINE_CACHE="$cache" WINE_RUN_DIR="$run_dir" WINE_PACKAGE_DIR="$package_dir"
export WINE_WINDOWS_CARGO="$(winpath "$windows_toolchain/bin/cargo.exe")"
export WINE_WINDOWS_PATH="$(winpath "$mingit/cmd");$(winpath "$mingit/usr/bin");$(winpath "$windows_toolchain/bin")"

for ((slot = 0; slot < jobs; slot++)); do WINEPREFIX="$cache/prefix-$slot" wineserver -p; done
xargs -a "$run_dir/binaries" -d '\n' -n 1 -P "$jobs" --process-slot-var=SLOT \
    bash -c 'run_one "${@: -1}" "${@:1:$#-1}"' _ "${test_args[@]}" || true
for ((slot = 0; slot < jobs; slot++)); do WINEPREFIX="$cache/prefix-$slot" wineserver -k || true; done

while read -r exe; do
    printf '     Running %s\n' "$exe"
    cat "$run_dir/$(basename "$exe" .exe).log"
done < "$run_dir/binaries"

if [ -s "$run_dir/failed" ]; then
    printf '\nwindows-wine: %s of %s test binaries failed under Wine:\n' \
        "$(wc -l < "$run_dir/failed")" "$(wc -l < "$run_dir/binaries")"
    while read -r exe; do
        name=$(basename "$exe" .exe)
        failed_tests=$(grep -h -- '\.\.\. FAILED$' "$run_dir/$name.log" | awk '!seen[$0]++' || true)
        if [ -n "$failed_tests" ]; then
            printf '%s\n' "$failed_tests" | sed "s/^test /  ${name%-*} /; s/ \.\.\. FAILED$//"
        else
            printf '  %s exited non-zero with no failing test; its output is above\n' "${name%-*}"
        fi
    done < "$run_dir/failed"
    exit 1
fi
printf '\nwindows-wine: %s test binaries passed under Wine\n' "$(wc -l < "$run_dir/binaries")"
