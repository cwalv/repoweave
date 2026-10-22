#!/usr/bin/env python3
"""Assemble the documentation site from this repository's release tags.

Usage: assemble-docs-site.py --repo <git checkout> --site-path <path> --out <new directory>

Every tag matching v* is a release, and must spell v<MAJOR>.<MINOR>.<PATCH>
with an optional -<PRERELEASE>. Each one's book is built from that tag's own
tree into <out>/<tag>/, with a version selector added to its pages. The site
root gets:

  index.html     a redirect to the default version
  404.html       a redirect from any address with no version in it to the same
                 address in the default version; under a version, not found
  versions.json  every version, highest first, and which one is the default
  install.sh     the default version's docs/install.sh, byte for byte

The default version is the highest stable version by semver precedence.
--site-path is the URL path the site root is served at, such as /repoweave/;
a 404 page is served at whatever address was missing, so it cannot find the
site root relative to itself. The checked-out tree is never read, so the
commit this runs from shapes the site's machinery but none of its content.

Needs Python 3.11+, git, and mdbook and mdbook-mermaid on PATH.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path

SELECTOR_SOURCE = Path(__file__).resolve().with_name("docs-site-version-selector.js")
SELECTOR_IN_BOOK = "version-selector.js"
NOT_FOUND_TEMPLATE = Path(__file__).resolve().with_name("docs-site-404.html")
NOT_FOUND_CONFIG = "SITE_CONFIG"

_IDENTIFIER = r"(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
RELEASE_TAG = re.compile(
    r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    rf"(?:-({_IDENTIFIER}(?:\.{_IDENTIFIER})*))?"
)


class Refusal(Exception):
    pass


@dataclass(frozen=True)
class Release:
    tag: str
    core: tuple[int, int, int]
    prerelease: tuple[str, ...]

    @property
    def stable(self) -> bool:
        return not self.prerelease

    def precedence(self):
        if self.stable:
            return (self.core, 1, ())
        return (
            self.core,
            0,
            tuple((0, int(i), "") if i.isdigit() else (1, 0, i) for i in self.prerelease),
        )


def parse_release(tag: str) -> Release:
    match = RELEASE_TAG.fullmatch(tag)
    if match is None:
        raise Refusal(
            f"tag {tag!r} matches v* but is not v<MAJOR>.<MINOR>.<PATCH>[-<PRERELEASE>]; "
            "every v* tag is published as a release, so rename or delete it"
        )
    major, minor, patch, prerelease = match.groups()
    return Release(
        tag,
        (int(major), int(minor), int(patch)),
        tuple(prerelease.split(".")) if prerelease else (),
    )


def default_release(releases: list[Release]) -> Release:
    stable = [r for r in releases if r.stable]
    if not stable:
        raise Refusal("no stable release tag, so the site root has no version to point at")
    return max(stable, key=Release.precedence)


def tool(name: str) -> str:
    found = shutil.which(name)
    if found is None:
        raise Refusal(f"{name} not found on PATH")
    return found


class Git:
    def __init__(self, repo: Path):
        self.repo = repo
        self.git = tool("git")
        listed = subprocess.run(
            [self.git, "rev-parse", "--local-env-vars"], capture_output=True, text=True, check=True
        )
        repo_locating = set(listed.stdout.split())
        self.env = {k: v for k, v in os.environ.items() if k not in repo_locating}

    def __call__(self, *args: str, env: dict | None = None) -> bytes:
        result = subprocess.run(
            [self.git, "-C", str(self.repo), *args],
            capture_output=True,
            env={**self.env, **(env or {})},
        )
        if result.returncode != 0:
            raise Refusal(
                f"git {' '.join(args)} failed: {result.stderr.decode(errors='replace').strip()}"
            )
        return result.stdout


def release_tags(git: Git) -> list[Release]:
    if git("rev-parse", "--is-shallow-repository").strip() == b"true":
        raise Refusal(
            f"{git.repo} is a shallow clone, so it can be missing release tags; "
            "fetch the full history (actions/checkout: fetch-depth: 0)"
        )
    tags = git("tag", "--list", "v*").decode(errors="replace").split()
    return sorted((parse_release(t) for t in tags), key=Release.precedence, reverse=True)


def export_tree(git: Git, release: Release, dest: Path, scratch: Path) -> None:
    index = {"GIT_INDEX_FILE": str(scratch / f"{release.tag}.index")}
    git("read-tree", f"refs/tags/{release.tag}^{{tree}}", env=index)
    git("checkout-index", "--all", f"--prefix={dest.as_posix()}/", env=index)


def run(argv: list[str], env: dict | None = None) -> None:
    code = subprocess.run(argv, env=env, stdout=sys.stderr).returncode
    if code != 0:
        raise Refusal(f"{' '.join(argv)} exited {code}")


def book_config(tree: Path) -> dict:
    return tomllib.loads((tree / "book.toml").read_text(encoding="utf-8"))


def build_book(tree: Path, dest: Path) -> None:
    if "mermaid" in book_config(tree).get("preprocessor", {}):
        run([tool("mdbook-mermaid"), "install", str(tree)])
    selector = tree / SELECTOR_IN_BOOK
    if selector.exists():
        raise Refusal(f"its tree already has a {SELECTOR_IN_BOOK} for the selector to replace")
    selector.write_bytes(SELECTOR_SOURCE.read_bytes())
    scripts = book_config(tree).get("output", {}).get("html", {}).get("additional-js", [])
    env = {
        **os.environ,
        "MDBOOK_OUTPUT__HTML__ADDITIONAL_JS": json.dumps([*scripts, SELECTOR_IN_BOOK]),
    }
    run([tool("mdbook"), "build", str(tree), "--dest-dir", str(dest)], env=env)


def redirect_page(tag: str) -> str:
    return f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>repoweave</title>
<meta http-equiv="refresh" content="0; url={tag}/">
<link rel="canonical" href="{tag}/">
</head>
<body>
<p>The documentation is at <a href="{tag}/">{tag}/</a>.</p>
</body>
</html>
"""


def not_found_page(site_path: str, default: Release, releases: list[Release]) -> str:
    template = NOT_FOUND_TEMPLATE.read_text(encoding="utf-8")
    if template.count(NOT_FOUND_CONFIG) != 1:
        raise Refusal(f"{NOT_FOUND_TEMPLATE} must spell {NOT_FOUND_CONFIG} exactly once")
    config = {"root": site_path, "default": default.tag, "versions": [r.tag for r in releases]}
    return template.replace(NOT_FOUND_CONFIG, json.dumps(config).replace("<", "\\u003c"))


def assemble(repo: Path, site_path: str, out: Path) -> None:
    if not (site_path.startswith("/") and site_path.endswith("/")):
        raise Refusal(f"--site-path {site_path!r} must start and end with /, such as /repoweave/")
    if out.exists():
        raise Refusal(
            f"{out} already exists; the site is built into a new directory, "
            "so nothing from an earlier build ships with it"
        )
    git = Git(repo)
    releases = release_tags(git)
    default = default_release(releases)
    not_found = not_found_page(site_path, default, releases)
    try:
        install_sh = git("cat-file", "blob", f"refs/tags/{default.tag}:docs/install.sh")
    except Refusal as e:
        raise Refusal(
            f"{default.tag} has no docs/install.sh to serve as the site's /install.sh: {e}"
        ) from e

    out.mkdir(parents=True)
    with tempfile.TemporaryDirectory(prefix="assemble-docs-site-") as scratch:
        for release in releases:
            tree = Path(scratch) / release.tag
            try:
                export_tree(git, release, tree, Path(scratch))
                build_book(tree, out / release.tag)
            except (Refusal, OSError, tomllib.TOMLDecodeError) as e:
                raise Refusal(f"{release.tag} does not build: {e}") from e
            print(f"built {release.tag}", file=sys.stderr)

    index = {
        "default": default.tag,
        "versions": [{"version": r.tag, "prerelease": not r.stable} for r in releases],
    }
    (out / "versions.json").write_text(json.dumps(index, indent=2) + "\n", encoding="utf-8")
    (out / "index.html").write_text(redirect_page(default.tag), encoding="utf-8")
    (out / "404.html").write_text(not_found, encoding="utf-8")
    (out / "install.sh").write_bytes(install_sh)
    print(f"site root -> {default.tag}", file=sys.stderr)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", required=True, type=Path)
    parser.add_argument("--site-path", required=True)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    try:
        assemble(args.repo.resolve(), args.site_path, args.out.resolve())
    except (Refusal, OSError) as e:
        print(f"assemble-docs-site: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
