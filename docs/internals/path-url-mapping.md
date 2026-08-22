# Path ↔ URL mapping

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

**Audience:** someone changing how rwv derives a canonical local path from a
clone source's identity, adds a registry, or extends the `rwv add --new`
creation surface. Every claim here is restated in full; nothing is cited by
line number, because line numbers rot and this document does not carry a
verification-base commit the way `branch-model.md` does. Symbol names are
checked against the tree at time of writing and are expected to survive
refactors better than a line would.

## The forward map is many-to-one, and that decides everything below

A clone source — a URL, a registry-qualified shorthand — determines exactly
one canonical local path. The reverse does not hold: a path alone (an
operator-typed string like `github/acme/fresh`) cannot in general be inverted
back to a URL, because two different sources can name the same path (a
registry's own shorthand and its fully-spelled URL both derive to the same
place) and because a path an operator invents has no source to invert at all.

This is why `rwv add --new` cannot work by inferring a URL from a typed path
— the inverse it would need is partial in a way no error type can paper
over — and why it instead asks a *registry* to mint an identity from
declared parameters, then places what comes back. That reframing is the
whole of what follows: **placement derives a path from an identity; nothing
derives an identity from a path.**

## `placement` — the sole producer of a derived path

```rust
pub fn placement(source: &RepoUrl) -> Option<RepoPath>;
```

in `src/registry.rs`. `None` when `source` names neither a URL a built-in
registry resolves nor a registry-qualified shorthand with a registry named —
a real absence, not a bug: a registry-less two-part shorthand
(`cwalv/repoweave`) has no default registry to derive a path under (ruled:
refuse, no default), and `rwv add`'s own refusal is what owns that case.

The public signature is a thin wrapper. The actual seam is:

```rust
pub(crate) enum PlacementError {
    NoMatch,
    Invalid(crate::manifest::RepoPathError),
}

pub(crate) fn placement_result(source: &RepoUrl) -> Result<RepoPath, PlacementError>;
```

`placement` is `placement_result(source).ok()`. The split exists because a
derived path can fail for two different reasons a caller sometimes needs to
tell apart: **no registry or host mapping applies at all** (`NoMatch` — the
caller renders a generic "unrecognized" refusal), or **a path was derived
but `RepoPath::new`'s own validation rejects it** (`Invalid`, carrying the
concrete `RepoPathError` — a backslash in an owner or repo segment, say). A
bare `Option` collapses those into one `None`, which is fine for most
callers but wrong for one: `rwv add`'s URL arm needs the concrete
`RepoPathError` to survive so `refusal.rs`'s anyhow-chain downcast can route
it to its own token (`backslash-in-repo-path`) instead of a generic
`no-matching-registry`. `run_add` matches on `placement_result` directly for
exactly this reason; every other caller uses the public `placement` wrapper.

### No other site constructs a *derived* path

C1's claim, precisely: **no site outside `registry.rs` computes where a
member *should* live from an identity.** This is narrower than "no verb
takes a path" — that version is both false of this tree and unenforceable,
because several sites legitimately hold a typed path as something other
than a layout decision:

| Role | What it means | Where it survives |
|---|---|---|
| **Lookup key** | A path compared against something that already exists (a manifest entry, a directory on disk); a miss refuses rather than creating. | `rwv add`'s local-path arm (the argument is only reached once already observed as a directory); `rwv remove` (matched against the manifest, a miss is `repo-not-in-manifest`). |
| **Observed location** | A report of where a checkout already sits, which rwv did not choose and cannot re-choose without moving files. | `run_add_from_local_path` (reads the URL from the clone's own `origin`, keeping the path the operator pointed at); `scan_repos_on_disk` (the containment walk). |
| **Project path** | The project repo's own path, `projects/<name>/` — not a manifest member, carries no registry segment for `placement` to derive from. | `check.rs`'s project-repo-key builders (`scan_phantom_merge_drivers`, the project-repo arm inside `branch_discipline_in_scope`, `collect_doctor_violations`'s hygiene-target list). |

A pinning test, `tests/derived_repo_path_single_producer_test.rs`, holds this
as a census over `src/`'s production lines: every `RepoPath::new(` call site
outside `registry.rs` is allowlisted by file with a stated count and a
justification for why that file's sites are lookup-key or observed rather
than derived; a new or grown site fails the pin until someone states which
it is.

**`run_add_new` is not on this list.** Before the creation surface below
existed, it took the operator's typed path as a *layout assertion* — the one
site in the tree that did. It no longer does: its `RepoPath` comes from
`placement(&plan.url)`, the same as every other derived site, once
`plan_creation` has minted a `RepoUrl` from the registry's own declared
parameters. Closing that gap was the point of the creation surface.

## The creation surface

Because the inverse of `placement` doesn't exist, `--new` cannot "invert a
path" — it asks a **registry** to create something and places what comes
back:

```rust
pub trait Registry {
    fn name(&self) -> &RegistryName;
    fn matches(&self, raw: &str) -> Option<RepoUrl>;
    fn clone_url(&self, id: &RepoId) -> Option<RepoUrl>;

    fn creation_params(&self) -> &[ParamSpec];
    fn plan_creation(&self, params: &ParamMap) -> Result<CreationPlan, CreationParamError>;
}

pub struct ParamSpec { pub name: &'static str, pub required: bool, pub help: &'static str }
pub struct ParamMap(/* name -> value, string-valued only */);

pub enum CreationParamError {
    Missing(Vec<&'static str>),
    Unrecognized(String),
}

pub struct CreationPlan { pub url: RepoUrl, pub vcs: VcsType, pub upstream: Upstream }

pub enum Upstream {
    InitBareAt(std::path::PathBuf),
    Named,
}
```

`creation_params()` and `plan_creation()` are the whole seam: the
missing-parameter refusal, the round-trip guard, and the generated
`rwv explain add` creation-parameter table all read the same slice, so a
registry that grows a parameter grows all three without an edit anywhere
else. `check_creation_params` in `registry.rs` is the one function every
`plan_creation` implementation routes through — it is what makes "declared
but absent" (`Missing`) and "supplied but undeclared" (`Unrecognized`)
checked identically everywhere rather than each registry re-deriving the
same two loops.

`Registry` stays object-safe: `creation_params`/`plan_creation` take and
return concrete types, not an associated type, so `Vec<Box<dyn Registry>>`
still holds every registry regardless of what parameters it declares. The
tradeoff this buys over a per-registry associated `CreateArgs` type: no
compile-time check that a caller passed the right shape of arguments, but
there is no in-code caller that could fail that check — the CLI is the only
producer of creation parameters, and it produces strings by construction.

### The creation address: a bare registry name, or a three-segment prefix

`rwv add <address> --new`. `<address>` is not a URL or a path — it selects a
registry and, optionally, fills a prefix of that registry's own parameter
surface:

- **A bare registry name** (`local`) — every parameter arrives via
  `--param`.
- **A three-segment shorthand** (`github/acme/fresh`) — the first segment
  names the registry; the remaining two fill `owner` and `repo` **as a
  prefix of whatever that registry declares**, not as its whole surface.
  `local` declares `root` as well, so `rwv add local/acme/fresh --new` fills
  `owner` and `repo` and still needs `--param root=<dir>` — the shorthand
  narrows what's missing rather than completing it. (This is the ruled
  answer to the question of whether the shorthand should be offered only by
  registries whose surface is *exactly* `(owner, repo)`: it is offered by
  every registry, filling only the prefix it can.)

The two spellings are told apart by whether the address contains a `/`,
decided before any registry is consulted: a bare `RegistryName` cannot
contain one (`RegistryName`s are minted only by `builtin_registries()`).
Anything that is not exactly one or exactly three non-empty segments refuses
`malformed-repo-path`.

Parameters merge from up to three sources — the address's own prefix,
repeated `--param name=value` flags, and one `--params-json` object of
string values — into a single `ParamMap` before any registry sees it. A name
supplied through more than one source refuses (`unusable-creation-param`):
the three spellings write into the same map, and a name given twice is a
real disagreement about intent, not a precedence question a silent default
could answer. `--params-json` values that are not JSON strings refuse the
same way — `ParamSpec` declares no types yet, so a number or object is
rejected rather than stringified.

**A fifth token was not minted for the multi-spelling conflict.** An earlier
pass gave it its own `RefusalKind` variant; the token count actually shipped
is four (`missing-creation-param`, `unusable-creation-param`,
`occupied-placement`, and the `rwv init --provider` refusal below), matching
what the adopted design's own acceptance criteria enumerate. The
multi-spelling conflict now shares `unusable-creation-param` with the
root-validity conditions below it — "this parameter cannot be used as
given" covers a value that fails validation and a value that was never
uniquely given in the first place equally well, and one token doing both
jobs is smaller surface than a fifth condition whose only content is
"conflict" would have bought.

### Two-sided creation, and why `local` is bare

A registry either mints a URL to something rwv cannot create (`Upstream::
Named` — every built-in domain registry: rwv has no HTTP client, so the
member is `git init`'d locally and its remote is set to the minted URL as an
intent), or it names something rwv must bring into existence first
(`Upstream::InitBareAt(path)` — `local`, the only such registry today).

`local`'s `plan_creation` mints `file://<root>/<owner>/<repo>` and a plan to
create a **bare** repository at that path, then the member is *cloned* from
it rather than `git init`'d — so the member's `origin` remote is real from
birth and `git push -u` works immediately, rather than pointing at a
directory rwv never touched. Bare, not a plain `git init`, because a
non-bare upstream refuses a push to its own checked-out branch — the very
first push from the member would fail against the thing creation was for.
`Vcs::init_bare_repo` is the one method this adds to the seam; `Vcs`
decides *how* a repository comes into being, the registry only decides
*that* one must exist and *where* — `local`'s `plan_creation` never calls
it.

`root` is resolved to an absolute path before it reaches `plan_creation` (so
the registry's own logic stays a pure function of strings, testable with a
fixture and no filesystem); the verb then checks the resolved
`Upstream::InitBareAt` path's grandparent — generically, not by
special-casing a parameter named `root` — against two conditions, both
`unusable-creation-param`: it must already exist (rwv creates
`<root>/<owner>/<repo>`, never `<root>` itself — a typo'd root that
silently materializes a tree is worse than a refusal), and it must resolve
outside the weave (an upstream inside the weave would be walked by the
containment scan, deletable by `rwv remove --delete` on the very member that
clones from it, and reported as an orphan by `rwv doctor` — the placement
path and the upstream path would collide at the degenerate case `root =
<weave>/local`).

**Two roots, one placement.** `placement` is a function of identity —
`owner` and `repo` — never of `root`, so two creations naming the same
`owner`/`repo` under different roots both derive the same path. The first
wins; a second with a *different* resolved URL refuses `occupied-placement`
rather than silently discarding the new root the way a bare
`manifest.contains_repo` check used to (an `eprintln!` and exit 0, invisible
to the refusal census). A second creation with the *same* URL is the
idempotent no-op it always was.

### `rwv init --provider`'s twin

`init --provider <registry>/<owner>` mints a URL from `clone_url` for the
*project* repo's remote — a different, narrower path that predates the
creation surface and was never re-pointed at `plan_creation` (it has no
`--root`, no parameter surface, and the "repo" half of its `RepoId` is the
project name, which may contain `/`). Once `local` became a registry with a
real `clone_url() -> None`, the `.expect("the only Registry impl always
supports clone URLs")` this call used to make became reachable — the
building-a-URL-for-`--provider local/<owner>` case has genuinely nothing to
mint from. It is now a refusal, `provider-cannot-mint-url`, naming the
exit that works: create the project without `--provider`, then set the
remote once the repository exists.

## The placement-disagreement doctor finding

`rwv doctor` compares `placement(entry.url)` against the manifest's own
recorded path for every entry (`scan_provenance` in `src/check.rs`, a third
`provenance` sub-kind beside `origin-url-mismatch` and
`lock-sha-unreachable`). Unlike its two siblings this is a pure manifest
comparison — it needs no on-disk clone, so it runs whether or not the repo
exists locally. Warning severity, report-only, no `--fix` arm: the repair is
either moving the checkout or re-keying the manifest entry, and which one is
right is the operator's call, the same reasoning `origin-url-mismatch`
already carries.

**Exemptions are declared by role, not inferred by survey.** Two roles never
reach the comparison at all:

- **`reference`** — a mirror may legitimately point at a URL that places
  somewhere other than the mirrored repo's own path (`file:///srv/mirrors/
  cargo.git` under `github/rust-lang/cargo`). This mirrors the exemption
  `origin-url-mismatch` already carries for the same role.
- **`fork`** — a fork entry may key its manifest path on the *upstream's*
  coordinates while its URL names the fork actually pushed to. This is real
  only because [`docs/reference/roles.md`](../reference/roles.md) §`fork`
  says so explicitly ("The path may name either") — before that sentence
  existed, the exemption would have cited a convention that said nothing
  about the path at all. The sentence, and the exemption it grounds, are one
  change.

The exemption is wider than the specific arrangement that justifies it — a
`fork` entry whose path is simply wrong is covered along with one whose path
deliberately names upstream — which is the right trade at warning severity
(a missing advisory) and a materially worse one at error severity (a false
refusal), should this escalate later. Escalation, if it comes, wants a
narrower per-entry declaration ("this divergence is intended") rather than
turning a role-shaped exemption into a hard gate.

## What this leaves undrivable, honestly

**The domain-registry half of a remote-publish exit is not testable without
a network.** `rwv add <path>` on a member whose remote has published no
default branch prints a hint that differs by circumstance: for a
`local`-registry member (an empty bare upstream rwv itself created) the
working exit is `git push -u <remote> <branch>`; for a domain-registry
member, the exit is to create the repository at the registry first. Only the
`local` half is driven by a test — the domain half needs a real remote this
suite does not have, and no test claims otherwise.

**`entry.url` is a fact for locally-created members and a promise for
remote ones.** A `local`-registry creation's URL points at a bare repository
rwv itself created, so a second machine's `rwv fetch` against that URL
genuinely materializes the member (driven in
`tests/add_remove_test.rs::add_new_local_registry_creates_a_bare_upstream_and_clones_the_member`).
A domain-registry creation's URL is written before anything exists there —
rwv has no HTTP client to create it — so nothing in the manifest currently
distinguishes "this URL is real" from "this URL is a hope". Revisit if any
verb that clones from `entry.url` (not only `rwv fetch` — `rwv sync`'s
`materialize_missing_repo` does too) fails on a `--new` member's URL in
practice; that is the trigger, not a specific verb.

## Related

- [`docs/explanation/joints/vcs-as-seam.md`](../explanation/joints/vcs-as-seam.md)
  — the `Vcs` trait's own boundary (rwv core vs. backend-specific mechanism).
  The registry/placement split above is an adjacent boundary, not the same
  one: registry owns identity, placement, and the creation parameter
  surface; `Vcs` owns init/clone/branch mechanics on whatever the registry
  decided to create. `CreationPlan.vcs` is the one field that crosses
  between them — set by the registry, read by the verb that writes
  `RepoEntry.vcs_type`.
- [`docs/explanation/joints/clone-topology.md`](../explanation/joints/clone-topology.md)
  — I1's canonical-store path (`<weave>/<repo_path>`) treats `repo_path` as
  given; `placement` is what derives it for every member rwv adds or
  creates.
- [`docs/explanation/joints/weave-root.md`](../explanation/joints/weave-root.md)
  — the weave-root shape contract (a directory containing `projects/`) that
  a walker uses instead of enumerating registry segment names, and why the
  registry set below being open-ended (a host no built-in registry
  recognises still derives a path under its own hostname) does not reopen
  that question.
- [`docs/reference/formats.md`](../reference/formats.md) — the operator-facing
  statement of the directory layout and the built-in registry set.
- [`docs/reference/roles.md`](../reference/roles.md) — the `fork` sentence
  the placement-disagreement exemption depends on.
- [`docs/internals/refusal-kinds.md`](./refusal-kinds.md) — the one-token,
  one-condition convention every refusal above follows.
