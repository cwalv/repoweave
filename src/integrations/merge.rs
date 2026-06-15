//! Shared merge helper for hybrid integrations.
//!
//! This module is the single home for the **hybrid file-ownership contract**
//! described in `docs/explanation/joints/file-ownership.md` (the joint doc).
//! It exists so the six hybrid integrations (cargo, uv, pnpm, go.work, npm,
//! vscode) share one implementation of:
//!
//! - the ownership marker (per-format placement, but uniform semantics);
//! - the marker-driven generate-vs-verify switch (rwv authors iff the key is
//!   marked or absent; if the user holds the pen, degrade to verify-and-warn);
//! - merge-activate (read-or-empty → set owned keys + marker → write);
//! - strip-deactivate (gate on marker → strip owned keys + marker → delete-if-
//!   -empty else write stripped).
//!
//! See the joint doc for the normative contract. This module is the
//! implementation; the per-integration ports consume it.
//!
//! # Per-format marker placement
//!
//! - **JSON** ([`JsonDoc`]): a top-level marker key whose value is an object
//!   like `{"managed": true}`. The marker key is parameterized by a
//!   [`JsonMarker`] type so that JSON-shaped integrations may pick different
//!   keys without sharing a top-level name. npm migrates to `x-repoweave`
//!   (the [`XRepoweaveMarker`] default), vscode keeps `rwv.generated` (the
//!   existing precedent, exposed as [`RwvGeneratedMarker`]).
//! - **TOML** ([`TomlDoc`]): a `# managed by rwv` *prefix decoration* on each
//!   owned key (position-independent — survives reordering). No top-level
//!   header is injected into user files.
//! - **YAML** ([`YamlDoc`]): a `# managed by repoweave` comment line above the
//!   managed block. Line-oriented; never round-trips through serde_yaml
//!   (which eats comments).
//! - **go.work** ([`GoWorkDoc`]): a `// managed by repoweave` comment line
//!   above the `use (…)` block. Custom line-region editor; preserves
//!   `replace`/`toolchain`/`godebug` byte-for-byte.
//!
//! # Key paths
//!
//! Owned keys are addressed as path segments ([`KeyPath`] = `Vec<String>`),
//! not dotted strings. This is so a literal-dot key (e.g. vscode's
//! `settings."files.exclude"`) is unambiguous: `["settings", "files.exclude"]`
//! is two segments, the second being the literal key `files.exclude`.
//!
//! # `OwnedValue` shape
//!
//! Deliberately minimal — only the value shapes the six hybrid integrations
//! actually need. [`OwnedValue::Object`] is for sub-key ownership (e.g. npm's
//! `workspaces.packages` while preserving `workspaces.nohoist`): when set
//! through `set_owned`, it merges into the existing map rather than replacing
//! the whole value. Leaf variants (`Bool`, `String`, `Array`) replace.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::path::Path;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A path to a managed key as a sequence of map segments.
///
/// Each segment is one map-key indirection. Dots inside a segment are literal
/// characters, not a separator — `KeyPath::from(["settings", "files.exclude"])`
/// addresses `obj["settings"]["files.exclude"]`, not
/// `obj["settings"]["files"]["exclude"]`.
pub type KeyPath = Vec<String>;

/// Convenience constructor for a [`KeyPath`] from a slice of borrowed strs.
pub fn keypath<I, S>(segments: I) -> KeyPath
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    segments.into_iter().map(Into::into).collect()
}

/// The ownership category for a managed key.
///
/// Controls how `merge_activate` and `strip_deactivate` treat the key:
///
/// - **`Author`**: rwv fully owns this key. `merge_activate` always writes
///   the value (overwriting whatever is on disk when the marker is present).
///   `strip_deactivate` removes this key. This is the "classic" ownership
///   category — the only one that existed before the richer categories below.
///
/// - **`DefaultOnly`**: rwv provides a default value but never overwrites once
///   the key is present. `merge_activate` sets the key only when it is absent
///   from the document (including on a fresh / empty file). If the key is
///   already present — whether or not it carries the rwv marker — the existing
///   value is preserved. `strip_deactivate` does **not** remove this key (it
///   is user-adjustable; stripping it would silently discard a choice the user
///   may have made). `verify()` treats `DefaultOnly` drift as CLEAN.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    /// rwv always writes and always strips this key.
    Author,
    /// rwv writes this key only when absent; never overwrites or strips it.
    DefaultOnly,
}

/// The cross-format owned-value type.
///
/// Only the shapes used by the six hybrid integrations are represented; this
/// is intentional, not a TODO. The `Object` variant merges into existing
/// content (sub-key ownership); leaf variants replace.
#[derive(Debug, Clone, PartialEq)]
pub enum OwnedValue {
    /// A boolean leaf. Replaces any existing value at the path.
    Bool(bool),
    /// A string leaf. Replaces any existing value at the path.
    String(String),
    /// A sorted array of strings (the helper does not sort — the caller is
    /// expected to pass already-sorted values for determinism). Replaces.
    Array(Vec<String>),
    /// A nested object whose sub-keys are individually owned. When set through
    /// `set_owned`, **merges** into the existing map at the path: sub-keys
    /// listed here are set; sub-keys not listed are preserved. The map itself
    /// is created if absent. This is how npm owns `workspaces.packages`
    /// without clobbering `workspaces.nohoist`.
    Object(BTreeMap<String, OwnedValue>),
    /// A TOML inline table `{ key = val, ... }`. Unlike `Object` (which
    /// produces a `[table]` section header in TOML), this produces a
    /// `key = { ... }` inline-table value. Used by uv's
    /// `[tool.uv.sources]` entries which **must** be inline tables
    /// (`server = { workspace = true }`) — a `[tool.uv.sources.server]`
    /// section header is not accepted by uv.
    ///
    /// For JSON/YAML/go.work this is equivalent to `Object` — the distinction
    /// is TOML-specific.
    InlineObject(BTreeMap<String, OwnedValue>),
}

impl OwnedValue {
    /// A convenience constructor for a sorted-string array. Sorts and
    /// deduplicates in place.
    pub fn sorted_array<I, S>(items: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut v: Vec<String> = items.into_iter().map(Into::into).collect();
        v.sort();
        v.dedup();
        OwnedValue::Array(v)
    }
}

/// The result of [`merge_activate`].
///
/// `authored` lists the owned keys this run wrote (the marker is present or
/// the key was absent — rwv held the pen). `deferred` lists the owned keys
/// this run skipped because the file held them without the marker (the user
/// took the pen — generate-vs-verify switch flipped to verify). The caller
/// emits `Severity::Warning` issues for each deferred key.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MergeResult {
    pub authored: Vec<KeyPath>,
    pub deferred: Vec<KeyPath>,
}

// ---------------------------------------------------------------------------
// The trait
// ---------------------------------------------------------------------------

/// The format-generic managed-document trait.
///
/// Each format impl (JSON/TOML/YAML/go.work) wraps its native parsed
/// representation and implements these operations. The free functions
/// [`merge_activate`] and [`strip_deactivate`] are written against this trait,
/// so the delete-if-empty / strip-only-owned invariants live exactly once.
///
/// # Marker semantics
///
/// `has_marker` / `set_marker` / `remove_marker` take an `owned_keys` slice
/// because some formats (TOML) place the marker as a per-key decoration; the
/// trait does not commit to a single sentinel location. JSON ignores the
/// slice (the marker is a top-level key); YAML/go.work ignore most segments
/// (the marker is a fixed comment line near the managed block).
///
/// # Key presence
///
/// `key_present` is what powers the verify-and-warn switch: if a managed key
/// is on disk but the marker is absent, the user took the pen and rwv must
/// not author the key.
pub trait ManagedDoc: Sized {
    /// Parse the document text. Bails loudly if malformed — never silently
    /// zeroes a file rwv does not fully own.
    fn parse(text: &str) -> Result<Self>;

    /// Construct an empty document.
    fn empty() -> Self;

    /// Does the document carry the ownership marker for *any* of the given
    /// owned keys? `owned_keys` is the universe of keys this integration
    /// declares ownership over; for formats with a single global marker
    /// (JSON), the slice is ignored.
    fn has_marker(&self, owned_keys: &[KeyPath]) -> bool;

    /// Apply the marker. For per-key-decor formats (TOML), decorates each of
    /// the given owned keys.
    fn set_marker(&mut self, owned_keys: &[KeyPath]);

    /// Remove the marker. For per-key-decor formats (TOML), removes the
    /// decoration from each of the given owned keys.
    fn remove_marker(&mut self, owned_keys: &[KeyPath]);

    /// Is the given key path present in the document? Used to detect
    /// user-holds-the-pen (key present but marker absent → defer).
    fn key_present(&self, key: &KeyPath) -> bool;

    /// Set an owned key. Leaf [`OwnedValue`] variants replace the value at
    /// the path; [`OwnedValue::Object`] merges into the existing map (sub-key
    /// ownership). Parent maps are created as needed.
    fn set_owned(&mut self, key: &KeyPath, value: &OwnedValue);

    /// Remove an owned key. Prunes now-empty parent maps. For an
    /// `OwnedValue::Object` ownership the caller must encode the sub-keys it
    /// owns; this method removes the whole path. (Sub-key strip — e.g. npm
    /// removing `workspaces.packages` while keeping `workspaces.nohoist` — is
    /// expressed by addressing the sub-key directly in `owned_keys` rather
    /// than the parent.)
    fn remove_owned(&mut self, key: &KeyPath);

    /// Is the document otherwise empty (after all owned keys and the marker
    /// have been removed)? Drives the delete-if-empty rule.
    fn is_empty(&self) -> bool;

    /// Serialize back to text.
    fn serialize(&self) -> Result<String>;
}

// ---------------------------------------------------------------------------
// Free functions: merge_activate and strip_deactivate
// ---------------------------------------------------------------------------

/// Activate the managed region of a hybrid file.
///
/// Semantics (the contract — joint doc §4):
/// - If the file is missing, start from an empty document.
/// - Parse; bail loudly if malformed (never silently zero a user file).
/// - For each `(key, ownership, value)` in `owned`:
///   - **`Ownership::Author`**: if the marker is **absent** and the key is
///     **present** on disk, defer (user holds the pen; verify-and-warn);
///     otherwise set the key (overwrite).
///   - **`Ownership::DefaultOnly`**: set the key only when it is **absent**
///     from the document. If the key is already present (regardless of the
///     marker), preserve the existing value — the user may have changed it
///     and that change is intentional. DefaultOnly keys are never deferred
///     and never reported in `MergeResult::authored`.
/// - If any `Author` key was authored, apply the marker on those keys.
/// - Write back. (Direct write — atomic-write-then-rename is a framework
///   concern; this helper does not assume one is wired.)
///
/// Returns a [`MergeResult`] describing which `Author` keys were authored vs
/// deferred, so the caller can emit `Severity::Warning` issues for deferred
/// ones. `DefaultOnly` keys do not appear in either list.
pub fn merge_activate<D: ManagedDoc>(
    path: &Path,
    owned: &[(KeyPath, Ownership, OwnedValue)],
) -> Result<MergeResult> {
    let mut doc = if path.exists() {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        D::parse(&text).with_context(|| format!("parsing {}", path.display()))?
    } else {
        D::empty()
    };

    // Only Author keys participate in the marker-present check and the
    // authored/deferred accounting.
    let author_key_paths: Vec<KeyPath> = owned
        .iter()
        .filter(|(_, o, _)| *o == Ownership::Author)
        .map(|(k, _, _)| k.clone())
        .collect();
    let marker_present = doc.has_marker(&author_key_paths);

    let mut result = MergeResult::default();
    for (key, ownership, value) in owned {
        match ownership {
            Ownership::Author => {
                if !marker_present && doc.key_present(key) {
                    // User holds the pen — degrade to verify-and-warn.
                    result.deferred.push(key.clone());
                    continue;
                }
                doc.set_owned(key, value);
                result.authored.push(key.clone());
            }
            Ownership::DefaultOnly => {
                // Write only when the key is absent. Never overwrite.
                if !doc.key_present(key) {
                    doc.set_owned(key, value);
                }
                // DefaultOnly keys are not reported in authored or deferred.
            }
        }
    }

    if !result.authored.is_empty() {
        doc.set_marker(&result.authored);
    }

    let text = doc.serialize()?;
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(result)
}

/// Deactivate the managed region of a hybrid file.
///
/// Semantics:
/// - If the file is missing: nothing to strip; `Ok(())`.
/// - Parse; bail loudly if malformed.
/// - If the marker is **absent** (against any of `owned_keys`): the user
///   holds the pen — leave the file alone.
/// - Otherwise: remove each owned key, remove the marker, prune empty
///   parents. If the document is then empty: delete the file. Else: write
///   the stripped document back (without the marker — the file becomes
///   hand-owned).
pub fn strip_deactivate<D: ManagedDoc>(path: &Path, owned_keys: &[KeyPath]) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut doc = D::parse(&text).with_context(|| format!("parsing {}", path.display()))?;

    if !doc.has_marker(owned_keys) {
        // User took the pen — never strip a hand-owned key.
        return Ok(());
    }

    for key in owned_keys {
        doc.remove_owned(key);
    }
    doc.remove_marker(owned_keys);

    if doc.is_empty() {
        std::fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
    } else {
        let text = doc.serialize()?;
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

// ===========================================================================
// JsonDoc — serde_json::Value-backed implementation
// ===========================================================================

/// A type-level JSON marker policy. Each impl declares the top-level marker
/// key (e.g. `x-repoweave`, `rwv.generated`).
pub trait JsonMarker {
    /// The marker key name.
    const KEY: &'static str;
}

/// The default JSON marker key, used by npm-workspaces.
///
/// Lives at `obj["x-repoweave"] = {"managed": true}`. `x-` prefix per the
/// json-extensions convention; reserves a namespace that real npm fields
/// will not collide with.
pub struct XRepoweaveMarker;
impl JsonMarker for XRepoweaveMarker {
    const KEY: &'static str = "x-repoweave";
}

/// The legacy JSON marker key, used by vscode-workspace.
///
/// Kept because the existing on-disk shape (`rwv.generated: true`) is the
/// vscode precedent the joint doc names; changing it would orphan every
/// committed `.code-workspace` from prior versions. (rwv is alpha and
/// migrations are operator-handled — but vscode's marker was already
/// explicit, so no migration is needed here.)
pub struct RwvGeneratedMarker;
impl JsonMarker for RwvGeneratedMarker {
    const KEY: &'static str = "rwv.generated";
}

/// JSON-backed managed document.
///
/// Wraps `serde_json::Value` (specifically an Object). The marker key is
/// parameterized by `M: JsonMarker` so npm and vscode share one impl with
/// different marker keys.
///
/// **Marker value:** the marker is stored as the object `{"managed": true}`
/// on the marker key for npm/`XRepoweaveMarker`. For `RwvGeneratedMarker`
/// (vscode), the marker value is the bool `true` — matching the existing
/// vscode shape. `has_marker` accepts either shape (bool true OR an object
/// whose `managed` field is true) to be tolerant of cross-version reads.
///
/// **Preserve_order:** if the workspace crate is compiled with the
/// `preserve_order` feature on `serde_json`, the JsonDoc preserves insertion
/// order automatically (serde_json::Map becomes IndexMap). Without it the
/// Map is alphabetized. Enabling the feature is C4's concern (npm migration),
/// not this bead's.
pub struct JsonDoc<M: JsonMarker = XRepoweaveMarker> {
    root: serde_json::Map<String, serde_json::Value>,
    _marker: PhantomData<M>,
}

impl<M: JsonMarker> JsonDoc<M> {
    fn marker_value() -> serde_json::Value {
        // The richer object form. has_marker tolerates a plain `true`.
        serde_json::json!({ "managed": true })
    }

    fn descend<'a>(
        root: &'a serde_json::Map<String, serde_json::Value>,
        key: &KeyPath,
    ) -> Option<&'a serde_json::Value> {
        let mut cur: &serde_json::Value = root.get(key.first()?)?;
        for seg in &key[1..] {
            cur = cur.as_object()?.get(seg)?;
        }
        Some(cur)
    }

    fn descend_mut<'a>(
        root: &'a mut serde_json::Map<String, serde_json::Value>,
        key: &KeyPath,
        create: bool,
    ) -> Option<&'a mut serde_json::Value> {
        if key.is_empty() {
            return None;
        }
        let first = &key[0];
        let entry = if create {
            root.entry(first.clone())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        } else {
            root.get_mut(first)?
        };
        let mut cur: &mut serde_json::Value = entry;
        for seg in &key[1..] {
            let next = if create {
                let map = match cur {
                    serde_json::Value::Object(m) => m,
                    other => {
                        // Replace non-object with an empty object — caller's
                        // `set_owned` semantics for nested paths.
                        *other = serde_json::Value::Object(serde_json::Map::new());
                        other.as_object_mut().unwrap()
                    }
                };
                map.entry(seg.clone())
                    .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
            } else {
                cur.as_object_mut()?.get_mut(seg)?
            };
            cur = next;
        }
        Some(cur)
    }

    fn owned_to_json(value: &OwnedValue) -> serde_json::Value {
        match value {
            OwnedValue::Bool(b) => serde_json::Value::Bool(*b),
            OwnedValue::String(s) => serde_json::Value::String(s.clone()),
            OwnedValue::Array(items) => serde_json::Value::Array(
                items
                    .iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            ),
            OwnedValue::Object(map) | OwnedValue::InlineObject(map) => {
                // InlineObject is TOML-specific; in JSON it is identical to Object.
                let mut out = serde_json::Map::new();
                for (k, v) in map {
                    out.insert(k.clone(), Self::owned_to_json(v));
                }
                serde_json::Value::Object(out)
            }
        }
    }

    fn merge_into(target: &mut serde_json::Value, value: &OwnedValue) {
        match value {
            OwnedValue::Object(map) => {
                // Sub-key merge: ensure target is an object, then set each
                // sub-key. Foreign sub-keys survive.
                if !target.is_object() {
                    *target = serde_json::Value::Object(serde_json::Map::new());
                }
                let target_map = target.as_object_mut().unwrap();
                for (k, v) in map {
                    match v {
                        OwnedValue::Object(_) => {
                            // Recurse: ensure sub-map exists and merge.
                            let entry = target_map.entry(k.clone()).or_insert_with(|| {
                                serde_json::Value::Object(serde_json::Map::new())
                            });
                            Self::merge_into(entry, v);
                        }
                        leaf => {
                            target_map.insert(k.clone(), Self::owned_to_json(leaf));
                        }
                    }
                }
            }
            leaf => {
                *target = Self::owned_to_json(leaf);
            }
        }
    }

    /// Remove the leaf segment at `key`, then prune now-empty parent maps.
    fn remove_at(root: &mut serde_json::Map<String, serde_json::Value>, key: &KeyPath) {
        Self::remove_leaf(root, key);
        // Prune empty intermediate maps from leaf-parent upward.
        for prune_depth in (1..key.len()).rev() {
            let prefix: KeyPath = key[..prune_depth].to_vec();
            let should_remove = match Self::descend(root, &prefix) {
                Some(serde_json::Value::Object(m)) => m.is_empty(),
                _ => false,
            };
            if should_remove {
                Self::remove_leaf(root, &prefix);
            } else {
                break;
            }
        }
    }

    fn remove_leaf(root: &mut serde_json::Map<String, serde_json::Value>, key: &KeyPath) {
        if key.is_empty() {
            return;
        }
        if key.len() == 1 {
            root.shift_remove_or_remove(&key[0]);
            return;
        }
        // Walk to parent in a single chain of mut borrows.
        let Some(first_val) = root.get_mut(&key[0]) else {
            return;
        };
        let mut cur: &mut serde_json::Value = first_val;
        for seg in &key[1..key.len() - 1] {
            let Some(next) = cur.as_object_mut().and_then(|m| m.get_mut(seg)) else {
                return;
            };
            cur = next;
        }
        let leaf_seg = &key[key.len() - 1];
        if let Some(map) = cur.as_object_mut() {
            map.shift_remove_or_remove(leaf_seg);
        }
    }
}

// Small extension trait so we can call shift_remove (preserve_order) or
// remove (BTreeMap) uniformly without depending on the feature flag.
trait MapRemove {
    fn shift_remove_or_remove(&mut self, key: &str) -> Option<serde_json::Value>;
}
impl MapRemove for serde_json::Map<String, serde_json::Value> {
    fn shift_remove_or_remove(&mut self, key: &str) -> Option<serde_json::Value> {
        // serde_json::Map's `remove` works for both backings.
        self.remove(key)
    }
}

impl<M: JsonMarker> ManagedDoc for JsonDoc<M> {
    fn parse(text: &str) -> Result<Self> {
        let trimmed = text.trim();
        let root: serde_json::Map<String, serde_json::Value> = if trimmed.is_empty() {
            serde_json::Map::new()
        } else {
            let v: serde_json::Value = serde_json::from_str(text).context("invalid JSON")?;
            match v {
                serde_json::Value::Object(m) => m,
                _ => anyhow::bail!("top-level JSON must be an object"),
            }
        };
        Ok(JsonDoc {
            root,
            _marker: PhantomData,
        })
    }

    fn empty() -> Self {
        JsonDoc {
            root: serde_json::Map::new(),
            _marker: PhantomData,
        }
    }

    fn has_marker(&self, _owned_keys: &[KeyPath]) -> bool {
        match self.root.get(M::KEY) {
            None => false,
            Some(serde_json::Value::Bool(b)) => *b,
            Some(serde_json::Value::Object(m)) => {
                m.get("managed").and_then(|v| v.as_bool()).unwrap_or(false)
            }
            _ => false,
        }
    }

    fn set_marker(&mut self, _owned_keys: &[KeyPath]) {
        self.root.insert(M::KEY.to_string(), Self::marker_value());
    }

    fn remove_marker(&mut self, _owned_keys: &[KeyPath]) {
        self.root.shift_remove_or_remove(M::KEY);
    }

    fn key_present(&self, key: &KeyPath) -> bool {
        Self::descend(&self.root, key).is_some()
    }

    fn set_owned(&mut self, key: &KeyPath, value: &OwnedValue) {
        if key.is_empty() {
            return;
        }
        if let OwnedValue::Object(_) = value {
            // Object value: walk/create the path; then merge sub-keys at the
            // target without clobbering foreign sub-keys.
            let target = Self::descend_mut(&mut self.root, key, true).unwrap();
            Self::merge_into(target, value);
        } else {
            // Leaf value: walk/create the parent path, then replace the leaf.
            if key.len() == 1 {
                self.root.insert(key[0].clone(), Self::owned_to_json(value));
                return;
            }
            let parent_path = key[..key.len() - 1].to_vec();
            let parent = Self::descend_mut(&mut self.root, &parent_path, true).unwrap();
            if !parent.is_object() {
                *parent = serde_json::Value::Object(serde_json::Map::new());
            }
            parent
                .as_object_mut()
                .unwrap()
                .insert(key[key.len() - 1].clone(), Self::owned_to_json(value));
        }
    }

    fn remove_owned(&mut self, key: &KeyPath) {
        Self::remove_at(&mut self.root, key);
    }

    fn is_empty(&self) -> bool {
        self.root.is_empty()
    }

    fn serialize(&self) -> Result<String> {
        let text = serde_json::to_string_pretty(&serde_json::Value::Object(self.root.clone()))
            .context("serializing JSON")?;
        Ok(text + "\n")
    }
}

// ===========================================================================
// TomlDoc — toml_edit::DocumentMut-backed implementation
// ===========================================================================

/// TOML-backed managed document.
///
/// Wraps [`toml_edit::DocumentMut`] so comments, key order, and inline-table
/// formatting survive round-trips. The ownership marker is a per-key
/// `# managed by rwv` *prefix decoration* (`Decor::set_prefix`) on each
/// owned key. Decorations are position-independent — they ride the key
/// through reorderings — and per-key placement avoids injecting a top-level
/// header into user files (cargo's `Cargo.toml`, uv's `pyproject.toml`).
///
/// **Marker recognition:** `has_marker` checks each owned key path for a
/// prefix containing the exact substring `"managed by rwv"`. The substring
/// (not equality) makes the check tolerant of comment-formatting variation
/// (`# managed by rwv` vs `#managed by rwv` vs trailing whitespace).
pub struct TomlDoc {
    doc: toml_edit::DocumentMut,
}

pub(crate) const TOML_MARKER_TEXT: &str = "managed by rwv";
const TOML_MARKER_PREFIX: &str = "# managed by rwv\n";

impl TomlDoc {
    /// Walk to the table at the given path (all segments except the last).
    /// Returns the parent table and the leaf-segment name.
    fn parent_and_leaf<'a>(
        doc: &'a mut toml_edit::DocumentMut,
        key: &KeyPath,
        create: bool,
    ) -> Option<(&'a mut toml_edit::Table, String)> {
        if key.is_empty() {
            return None;
        }
        let leaf = key.last().unwrap().clone();
        let mut table: &mut toml_edit::Table = doc.as_table_mut();
        for seg in &key[..key.len() - 1] {
            if create && !table.contains_key(seg) {
                let mut t = toml_edit::Table::new();
                t.set_implicit(true);
                table.insert(seg, toml_edit::Item::Table(t));
            }
            let item = table.get_mut(seg)?;
            match item {
                toml_edit::Item::Table(t) => table = t,
                _ => return None,
            }
        }
        Some((table, leaf))
    }

    fn owned_to_toml_item(value: &OwnedValue) -> toml_edit::Item {
        match value {
            OwnedValue::Bool(b) => toml_edit::value(*b),
            OwnedValue::String(s) => toml_edit::value(s.as_str()),
            OwnedValue::Array(items) => {
                let mut arr = toml_edit::Array::new();
                for s in items {
                    arr.push(s.as_str());
                }
                toml_edit::value(arr)
            }
            OwnedValue::Object(map) => {
                let mut t = toml_edit::Table::new();
                for (k, v) in map {
                    t.insert(k, Self::owned_to_toml_item(v));
                }
                toml_edit::Item::Table(t)
            }
            OwnedValue::InlineObject(map) => {
                // Produces `key = { field = val }` — an inline table, NOT a
                // `[table]` section header. Required for uv's
                // `[tool.uv.sources]` entries.
                let mut t = toml_edit::InlineTable::new();
                for (k, v) in map {
                    let val = match v {
                        OwnedValue::Bool(b) => toml_edit::Value::from(*b),
                        OwnedValue::String(s) => toml_edit::Value::from(s.as_str()),
                        _ => continue, // nested inline objects not needed
                    };
                    t.insert(k, val);
                }
                toml_edit::Item::Value(toml_edit::Value::InlineTable(t))
            }
        }
    }

    fn merge_table(target: &mut toml_edit::Table, map: &BTreeMap<String, OwnedValue>) {
        for (k, v) in map {
            match v {
                OwnedValue::Object(sub_map) => {
                    if !target.contains_key(k) {
                        target.insert(k, toml_edit::Item::Table(toml_edit::Table::new()));
                    }
                    if let Some(toml_edit::Item::Table(sub_table)) = target.get_mut(k) {
                        Self::merge_table(sub_table, sub_map);
                    } else {
                        // Existing item is not a table — replace with a table
                        // and merge into it.
                        let mut t = toml_edit::Table::new();
                        Self::merge_table(&mut t, sub_map);
                        target.insert(k, toml_edit::Item::Table(t));
                    }
                }
                leaf => {
                    target.insert(k, Self::owned_to_toml_item(leaf));
                }
            }
        }
    }

    /// Apply the `# managed by rwv` decoration to a specific key, idempotently.
    fn decorate_key(table: &mut toml_edit::Table, leaf: &str) {
        let Some(mut key_mut) = table.key_mut(leaf) else {
            return;
        };
        let decor = key_mut.leaf_decor_mut();
        let existing = decor.prefix().and_then(|r| r.as_str()).unwrap_or("");
        if existing.contains(TOML_MARKER_TEXT) {
            return;
        }
        // Preserve any existing prefix (comments/blank lines the user wrote).
        let new = if existing.is_empty() {
            TOML_MARKER_PREFIX.to_string()
        } else if existing.ends_with('\n') {
            format!("{}{}", existing, TOML_MARKER_PREFIX)
        } else {
            format!("{}\n{}", existing, TOML_MARKER_PREFIX)
        };
        decor.set_prefix(new);
    }

    fn key_has_marker(table: &toml_edit::Table, leaf: &str) -> bool {
        let Some(key_mut) = table.key(leaf) else {
            return false;
        };
        let decor = key_mut.leaf_decor();
        decor
            .prefix()
            .and_then(|r| r.as_str())
            .is_some_and(|s| s.contains(TOML_MARKER_TEXT))
    }

    fn undecorate_key(table: &mut toml_edit::Table, leaf: &str) {
        let Some(mut key_mut) = table.key_mut(leaf) else {
            return;
        };
        let decor = key_mut.leaf_decor_mut();
        let existing = decor.prefix().and_then(|r| r.as_str()).unwrap_or("");
        if existing.is_empty() {
            return;
        }
        // Strip lines containing the marker text.
        let stripped: String = existing
            .lines()
            .filter(|line| !line.contains(TOML_MARKER_TEXT))
            .collect::<Vec<_>>()
            .join("\n");
        // Preserve trailing newline if there were multiple lines.
        let stripped = if existing.ends_with('\n') && !stripped.is_empty() {
            format!("{stripped}\n")
        } else {
            stripped
        };
        decor.set_prefix(stripped);
    }
}

impl ManagedDoc for TomlDoc {
    fn parse(text: &str) -> Result<Self> {
        let doc: toml_edit::DocumentMut = text.parse().context("invalid TOML")?;
        Ok(TomlDoc { doc })
    }

    fn empty() -> Self {
        TomlDoc {
            doc: toml_edit::DocumentMut::new(),
        }
    }

    fn has_marker(&self, owned_keys: &[KeyPath]) -> bool {
        // Marker is per-key — present if ANY owned key carries the decor.
        for key in owned_keys {
            if key.is_empty() {
                continue;
            }
            // Walk to the parent table (read-only).
            let mut table: &toml_edit::Table = self.doc.as_table();
            let mut walked = true;
            for seg in &key[..key.len() - 1] {
                match table.get(seg) {
                    Some(toml_edit::Item::Table(t)) => table = t,
                    _ => {
                        walked = false;
                        break;
                    }
                }
            }
            if !walked {
                continue;
            }
            let leaf = key.last().unwrap();
            if Self::key_has_marker(table, leaf) {
                return true;
            }
        }
        false
    }

    fn set_marker(&mut self, owned_keys: &[KeyPath]) {
        for key in owned_keys {
            if let Some((table, leaf)) = Self::parent_and_leaf(&mut self.doc, key, false) {
                Self::decorate_key(table, &leaf);
            }
        }
    }

    fn remove_marker(&mut self, owned_keys: &[KeyPath]) {
        for key in owned_keys {
            if let Some((table, leaf)) = Self::parent_and_leaf(&mut self.doc, key, false) {
                Self::undecorate_key(table, &leaf);
            }
        }
    }

    fn key_present(&self, key: &KeyPath) -> bool {
        if key.is_empty() {
            return false;
        }
        let mut table: &toml_edit::Table = self.doc.as_table();
        for seg in &key[..key.len() - 1] {
            match table.get(seg) {
                Some(toml_edit::Item::Table(t)) => table = t,
                _ => return false,
            }
        }
        table.contains_key(key.last().unwrap())
    }

    fn set_owned(&mut self, key: &KeyPath, value: &OwnedValue) {
        let Some((table, leaf)) = Self::parent_and_leaf(&mut self.doc, key, true) else {
            return;
        };
        match value {
            OwnedValue::Object(map) => {
                // Sub-key merge: ensure sub-table exists and merge fields.
                if !matches!(table.get(&leaf), Some(toml_edit::Item::Table(_))) {
                    table.insert(&leaf, toml_edit::Item::Table(toml_edit::Table::new()));
                }
                if let Some(toml_edit::Item::Table(sub)) = table.get_mut(&leaf) {
                    Self::merge_table(sub, map);
                }
            }
            leaf_value => {
                table.insert(&leaf, Self::owned_to_toml_item(leaf_value));
            }
        }
    }

    fn remove_owned(&mut self, key: &KeyPath) {
        if key.is_empty() {
            return;
        }
        // Walk to parent and remove leaf.
        let Some((table, leaf)) = Self::parent_and_leaf(&mut self.doc, key, false) else {
            return;
        };
        table.remove(&leaf);
        // Prune empty intermediate tables.
        for prune_depth in (1..key.len()).rev() {
            let prefix = &key[..prune_depth];
            let mut should_remove = false;
            {
                let mut table: &toml_edit::Table = self.doc.as_table();
                let mut walked = true;
                for seg in &prefix[..prefix.len() - 1] {
                    match table.get(seg) {
                        Some(toml_edit::Item::Table(t)) => table = t,
                        _ => {
                            walked = false;
                            break;
                        }
                    }
                }
                if walked {
                    if let Some(toml_edit::Item::Table(t)) = table.get(prefix.last().unwrap()) {
                        if t.is_empty() {
                            should_remove = true;
                        }
                    }
                }
            }
            if should_remove {
                let prefix_vec: KeyPath = prefix.to_vec();
                if let Some((t, l)) = Self::parent_and_leaf(&mut self.doc, &prefix_vec, false) {
                    t.remove(&l);
                }
            } else {
                break;
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.doc.as_table().is_empty()
    }

    fn serialize(&self) -> Result<String> {
        Ok(self.doc.to_string())
    }
}

// ===========================================================================
// YamlDoc — line-oriented YAML editor (for pnpm-workspace.yaml)
// ===========================================================================

/// YAML-backed managed document.
///
/// Wraps the raw text and edits it line-by-line. Does **not** round-trip
/// through serde_yaml — serde_yaml 0.9 drops all comments, which is fatal
/// for pnpm's `catalog:` rationale notes / `overrides:` justifications.
///
/// **Scope:** the only YAML hybrid integration is pnpm-workspaces, which
/// owns a single key — `packages: [...]`. The YamlDoc encodes only that
/// shape (a top-level key with an array-of-strings value, written in
/// block-list form). Other owned keys are rejected at `set_owned` time
/// (silently ignored — the caller's contract is documented).
///
/// **Marker:** the line `# managed by repoweave` immediately above the
/// managed `packages:` block.
pub struct YamlDoc {
    text: String,
    /// The owned key and its array value, set via `set_owned`. Only one
    /// owned key is supported (pnpm's `packages`).
    pending: Option<(String, Vec<String>)>,
    /// Whether the marker has been requested.
    marker_set: bool,
    /// Whether to remove the marker on serialize (for strip).
    marker_remove: bool,
}

const YAML_MARKER_LINE: &str = "# managed by repoweave";

impl YamlDoc {
    /// Locate the line range of a top-level `key:` block in `text`.
    /// Returns `(start_line, end_line_exclusive, marker_line_index)`:
    /// - `start_line`: the line index of `key:` (or `key: [...]`).
    /// - `end_line_exclusive`: the line index one past the last item of the
    ///   block.
    /// - `marker_line_index`: the line index of the marker comment, if it
    ///   sits immediately above the block (possibly with blank lines between).
    fn locate_block(text: &str, key: &str) -> Option<(usize, usize, Option<usize>)> {
        let lines: Vec<&str> = text.lines().collect();
        let prefix = format!("{key}:");
        let mut start: Option<usize> = None;
        for (i, line) in lines.iter().enumerate() {
            let trimmed_leading = line.trim_start();
            // Require column-0 (top-level) — pnpm-workspace.yaml is shallow.
            if !line.starts_with(&prefix) {
                continue;
            }
            // Confirm it's actually `key:` not e.g. `keysomething:`.
            let after = &line[prefix.len()..];
            if !after.is_empty() && !after.starts_with(|c: char| c.is_whitespace() || c == '\r') {
                continue;
            }
            let _ = trimmed_leading;
            start = Some(i);
            break;
        }
        let start = start?;
        // Find the end: next non-indented, non-comment, non-blank line.
        let mut end = lines.len();
        for (j, line) in lines.iter().enumerate().skip(start + 1) {
            if line.is_empty() {
                continue;
            }
            let first_byte = line.as_bytes()[0];
            if first_byte == b' ' || first_byte == b'\t' || first_byte == b'#' {
                continue;
            }
            end = j;
            break;
        }
        // Trim trailing blank/comment lines (they belong to the next block).
        while end > start + 1 {
            let prev = lines[end - 1];
            if prev.is_empty() {
                end -= 1;
                continue;
            }
            break;
        }
        // Find marker line: scan upward from start looking for the marker,
        // allowing blank lines between.
        let mut marker_idx: Option<usize> = None;
        let mut k = start;
        while k > 0 {
            k -= 1;
            let line = lines[k];
            if line.is_empty() {
                continue;
            }
            if line.trim() == YAML_MARKER_LINE {
                marker_idx = Some(k);
            }
            break;
        }
        Some((start, end, marker_idx))
    }
}

impl ManagedDoc for YamlDoc {
    fn parse(text: &str) -> Result<Self> {
        // YAML is not validated — pnpm-workspace.yaml is shallow and a parse
        // check would require pulling in a YAML parser. We accept any text;
        // malformed YAML is caught when the consumer (pnpm) reads the file.
        // **Note:** the contract says bail on malformed. For pnpm we relax —
        // a strict parse would force serde_yaml which kills comments. Future
        // hardening: add a lightweight column-aware validator.
        Ok(YamlDoc {
            text: text.to_string(),
            pending: None,
            marker_set: false,
            marker_remove: false,
        })
    }

    fn empty() -> Self {
        YamlDoc {
            text: String::new(),
            pending: None,
            marker_set: false,
            marker_remove: false,
        }
    }

    fn has_marker(&self, owned_keys: &[KeyPath]) -> bool {
        // Marker present iff a managed key block has the marker line above.
        for key in owned_keys {
            let Some(seg) = key.first() else {
                continue;
            };
            if let Some((_start, _end, marker_idx)) = Self::locate_block(&self.text, seg) {
                if marker_idx.is_some() {
                    return true;
                }
            }
        }
        false
    }

    fn set_marker(&mut self, _owned_keys: &[KeyPath]) {
        self.marker_set = true;
    }

    fn remove_marker(&mut self, _owned_keys: &[KeyPath]) {
        self.marker_remove = true;
    }

    fn key_present(&self, key: &KeyPath) -> bool {
        let Some(seg) = key.first() else {
            return false;
        };
        Self::locate_block(&self.text, seg).is_some()
    }

    fn set_owned(&mut self, key: &KeyPath, value: &OwnedValue) {
        // Only top-level array-of-strings keys are supported (pnpm `packages`).
        if key.len() != 1 {
            return;
        }
        if let OwnedValue::Array(items) = value {
            self.pending = Some((key[0].clone(), items.clone()));
        }
    }

    fn remove_owned(&mut self, key: &KeyPath) {
        let Some(seg) = key.first() else {
            return;
        };
        let Some((start, end, marker_idx)) = Self::locate_block(&self.text, seg) else {
            return;
        };
        // Remove lines [marker_idx..end) (or [start..end) if no marker).
        let lines: Vec<&str> = self.text.lines().collect();
        let trailing_newline = self.text.ends_with('\n');
        let remove_from = marker_idx.unwrap_or(start);
        let kept: Vec<&str> = lines
            .iter()
            .enumerate()
            .filter(|(i, _)| *i < remove_from || *i >= end)
            .map(|(_, s)| *s)
            .collect();
        let mut joined = kept.join("\n");
        if trailing_newline && !joined.is_empty() {
            joined.push('\n');
        }
        // Trim trailing blank line that may be left behind.
        while joined.ends_with("\n\n") {
            joined.pop();
        }
        self.text = joined;
    }

    fn is_empty(&self) -> bool {
        self.text
            .lines()
            .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#'))
    }

    fn serialize(&self) -> Result<String> {
        // Bake any pending owned key into the text.
        if let Some((key, items)) = &self.pending {
            let mut new_text = self.text.clone();
            // Build the replacement block.
            let mut block = String::new();
            if self.marker_set {
                block.push_str(YAML_MARKER_LINE);
                block.push('\n');
            }
            block.push_str(key);
            block.push_str(":\n");
            for item in items {
                block.push_str("  - ");
                block.push_str(item);
                block.push('\n');
            }
            if let Some((start, end, marker_idx)) = Self::locate_block(&new_text, key) {
                // Replace existing block (with marker if present).
                let lines: Vec<&str> = new_text.lines().collect();
                let replace_from = marker_idx.unwrap_or(start);
                let mut out = String::new();
                for (i, line) in lines.iter().enumerate() {
                    if i == replace_from {
                        out.push_str(&block);
                    }
                    if i < replace_from || i >= end {
                        out.push_str(line);
                        out.push('\n');
                    }
                }
                if replace_from >= lines.len() {
                    out.push_str(&block);
                }
                new_text = out;
            } else {
                // Append the block at the end.
                if !new_text.is_empty() && !new_text.ends_with('\n') {
                    new_text.push('\n');
                }
                if !new_text.is_empty() {
                    // Separate the new block from existing content with a blank line.
                    new_text.push('\n');
                }
                new_text.push_str(&block);
            }
            return Ok(new_text);
        }
        // Strip path: optionally remove a stray marker line not adjacent to a
        // managed block (defensive — `remove_owned` already removes it when
        // it knows the block range).
        if self.marker_remove {
            let kept: Vec<&str> = self
                .text
                .lines()
                .filter(|line| line.trim() != YAML_MARKER_LINE)
                .collect();
            let mut joined = kept.join("\n");
            if self.text.ends_with('\n') && !joined.is_empty() {
                joined.push('\n');
            }
            return Ok(joined);
        }
        Ok(self.text.clone())
    }
}

// ===========================================================================
// GoWorkDoc — line-region editor for go.work
// ===========================================================================

/// go.work-backed managed document.
///
/// Manages two regions of a go.work file:
/// - the `use (…)` block (the set of module paths) and the optional single
///   `use path` form;
/// - the leading `go <version>` line (only when the config sets it — this
///   matches the plan's "never emit hardcoded `go 1.21` over a user's
///   `go 1.26`").
///
/// Does **not** model `replace (…)`, `toolchain`, `godebug`, comments, or
/// any other directive — those are user content and are preserved byte-for-
/// byte by editing only the targeted regions.
///
/// **Marker:** `// managed by repoweave` comment line immediately above the
/// `use (…)` block.
pub struct GoWorkDoc {
    text: String,
    /// Pending use entries set via `set_owned(["use"], Array([...]))`.
    pending_use: Option<Vec<String>>,
    /// Pending go-version set via `set_owned(["go"], String(...))`.
    pending_go: Option<String>,
    marker_set: bool,
    marker_remove: bool,
}

const GO_MARKER_LINE: &str = "// managed by repoweave";

impl GoWorkDoc {
    /// Find the line index and end-line of an existing `use (…)` block.
    /// Returns `(use_start, use_end_exclusive, marker_idx_if_above)`.
    fn locate_use_block(text: &str) -> Option<(usize, usize, Option<usize>)> {
        let lines: Vec<&str> = text.lines().collect();
        let mut start: Option<usize> = None;
        let mut multiline = false;
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("use (") || trimmed == "use(" {
                start = Some(i);
                multiline = true;
                break;
            }
            if trimmed.starts_with("use ") && !trimmed.contains('(') {
                // Single-entry form: `use ./foo`. Treat as one-line block.
                start = Some(i);
                multiline = false;
                break;
            }
        }
        let start = start?;
        let end = if multiline {
            // Find matching `)`.
            let mut e = lines.len();
            for (j, line) in lines.iter().enumerate().skip(start + 1) {
                if line.trim().starts_with(')') {
                    e = j + 1;
                    break;
                }
            }
            e
        } else {
            start + 1
        };
        // Marker line: nearest non-blank above.
        let mut marker_idx: Option<usize> = None;
        let mut k = start;
        while k > 0 {
            k -= 1;
            let line = lines[k];
            if line.trim().is_empty() {
                continue;
            }
            if line.trim() == GO_MARKER_LINE {
                marker_idx = Some(k);
            }
            break;
        }
        Some((start, end, marker_idx))
    }

    /// Find the `go <version>` line index.
    fn locate_go_line(text: &str) -> Option<usize> {
        text.lines().enumerate().find_map(|(i, line)| {
            let trimmed = line.trim();
            if trimmed.starts_with("go ")
                && trimmed.chars().nth(3).is_some_and(|c| c.is_ascii_digit())
            {
                Some(i)
            } else {
                None
            }
        })
    }
}

impl ManagedDoc for GoWorkDoc {
    fn parse(text: &str) -> Result<Self> {
        // No formal parse — go.work syntax is simple and we only edit known
        // regions. A go.work without a valid `go` line is still legal input
        // (the activate-then-write path may add one).
        Ok(GoWorkDoc {
            text: text.to_string(),
            pending_use: None,
            pending_go: None,
            marker_set: false,
            marker_remove: false,
        })
    }

    fn empty() -> Self {
        GoWorkDoc {
            text: String::new(),
            pending_use: None,
            pending_go: None,
            marker_set: false,
            marker_remove: false,
        }
    }

    fn has_marker(&self, _owned_keys: &[KeyPath]) -> bool {
        Self::locate_use_block(&self.text)
            .and_then(|(_, _, m)| m)
            .is_some()
    }

    fn set_marker(&mut self, _owned_keys: &[KeyPath]) {
        self.marker_set = true;
    }

    fn remove_marker(&mut self, _owned_keys: &[KeyPath]) {
        self.marker_remove = true;
    }

    fn key_present(&self, key: &KeyPath) -> bool {
        match key.first().map(|s| s.as_str()) {
            Some("use") => Self::locate_use_block(&self.text).is_some(),
            Some("go") => Self::locate_go_line(&self.text).is_some(),
            _ => false,
        }
    }

    fn set_owned(&mut self, key: &KeyPath, value: &OwnedValue) {
        match (key.first().map(|s| s.as_str()), value) {
            (Some("use"), OwnedValue::Array(items)) => {
                self.pending_use = Some(items.clone());
            }
            (Some("go"), OwnedValue::String(s)) => {
                self.pending_go = Some(s.clone());
            }
            _ => {}
        }
    }

    fn remove_owned(&mut self, key: &KeyPath) {
        match key.first().map(|s| s.as_str()) {
            Some("use") => {
                if let Some((start, end, marker_idx)) = Self::locate_use_block(&self.text) {
                    let lines: Vec<&str> = self.text.lines().collect();
                    let trailing_newline = self.text.ends_with('\n');
                    let remove_from = marker_idx.unwrap_or(start);
                    let kept: Vec<&str> = lines
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| *i < remove_from || *i >= end)
                        .map(|(_, s)| *s)
                        .collect();
                    let mut joined = kept.join("\n");
                    if trailing_newline && !joined.is_empty() {
                        joined.push('\n');
                    }
                    while joined.ends_with("\n\n") {
                        joined.pop();
                    }
                    self.text = joined;
                }
            }
            Some("go") => {
                // The plan says: never strip `go` on deactivate unless the
                // file is otherwise empty. We honor that by only removing
                // the `go` line if all other managed regions are also gone.
                // For simplicity here we DO remove it; the helper's
                // delete-if-empty check above handles the "purely-rwv" case.
                // For mixed cases the caller would not include "go" in
                // owned_keys on strip. Documented in the joint doc for go.work.
                if let Some(i) = Self::locate_go_line(&self.text) {
                    let lines: Vec<&str> = self.text.lines().collect();
                    let trailing_newline = self.text.ends_with('\n');
                    let kept: Vec<&str> = lines
                        .iter()
                        .enumerate()
                        .filter(|(j, _)| *j != i)
                        .map(|(_, s)| *s)
                        .collect();
                    let mut joined = kept.join("\n");
                    if trailing_newline && !joined.is_empty() {
                        joined.push('\n');
                    }
                    self.text = joined;
                }
            }
            _ => {}
        }
    }

    fn is_empty(&self) -> bool {
        // "Empty" means: no user-authored content remains after the owned
        // `use` region has been stripped.  Per the bead spec the delete-if-
        // empty predicate is: no `use` entries AND no `replace`/`godebug`/
        // non-comment lines beyond `go`/`toolchain`/whitespace.
        //
        // In other words, a file consisting solely of `go`, `toolchain`,
        // blank lines, and `//`-comments is considered empty and should be
        // deleted.  A file that still carries a `replace`, `godebug`, or any
        // other directive is non-empty and must survive.
        self.text.lines().all(|line| {
            let t = line.trim();
            t.is_empty()
                || t.starts_with("//")
                || t.starts_with("go ")
                || t == "go"
                || t.starts_with("toolchain ")
                || t == "toolchain"
        })
    }

    fn serialize(&self) -> Result<String> {
        let mut text = self.text.clone();

        // Apply pending `go <version>`.
        if let Some(go_ver) = &self.pending_go {
            let new_line = format!("go {go_ver}");
            if let Some(i) = Self::locate_go_line(&text) {
                let lines: Vec<&str> = text.lines().collect();
                let trailing_newline = text.ends_with('\n');
                let mut out = String::new();
                for (j, line) in lines.iter().enumerate() {
                    if j == i {
                        out.push_str(&new_line);
                    } else {
                        out.push_str(line);
                    }
                    out.push('\n');
                }
                if !trailing_newline {
                    out.pop();
                }
                text = out;
            } else {
                // Prepend.
                let mut out = String::new();
                out.push_str(&new_line);
                out.push('\n');
                out.push_str(&text);
                text = out;
            }
        }

        // Apply pending use block.
        if let Some(items) = &self.pending_use {
            let mut block = String::new();
            if self.marker_set {
                block.push_str(GO_MARKER_LINE);
                block.push('\n');
            }
            if items.len() == 1 {
                block.push_str("use ");
                block.push_str(&items[0]);
                block.push('\n');
            } else {
                block.push_str("use (\n");
                for item in items {
                    block.push('\t');
                    block.push_str(item);
                    block.push('\n');
                }
                block.push_str(")\n");
            }

            if let Some((start, end, marker_idx)) = Self::locate_use_block(&text) {
                let lines: Vec<&str> = text.lines().collect();
                let replace_from = marker_idx.unwrap_or(start);
                let mut out = String::new();
                for (i, line) in lines.iter().enumerate() {
                    if i == replace_from {
                        out.push_str(&block);
                    }
                    if i < replace_from || i >= end {
                        out.push_str(line);
                        out.push('\n');
                    }
                }
                text = out;
            } else {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&block);
            }
        }

        // Strip path: defensive marker-line cleanup if the use-block was
        // already removed but a stray marker survives.
        if self.marker_remove && self.pending_use.is_none() {
            let kept: Vec<&str> = text
                .lines()
                .filter(|line| line.trim() != GO_MARKER_LINE)
                .collect();
            let mut joined = kept.join("\n");
            if text.ends_with('\n') && !joined.is_empty() {
                joined.push('\n');
            }
            text = joined;
        }

        Ok(text)
    }
}

// ===========================================================================
// Shared parse helpers
// ===========================================================================

/// Walk `doc` along `path` segments and return the array as `Vec<String>`.
///
/// Returns `None` if the path does not exist, is not an array, or contains
/// non-string elements. Used by TOML-backed `verify()` implementations to read
/// back on-disk arrays for DRIFT comparison.
pub(crate) fn toml_array_strings(
    doc: &toml_edit::DocumentMut,
    path: &[&str],
) -> Option<Vec<String>> {
    if path.is_empty() {
        return None;
    }
    let mut table: &toml_edit::Table = doc.as_table();
    for seg in &path[..path.len() - 1] {
        match table.get(seg) {
            Some(toml_edit::Item::Table(t)) => table = t,
            _ => return None,
        }
    }
    let item = table.get(path.last().unwrap())?;
    let arr = item.as_array()?;
    // Collect all string elements; if any element is not a string, return None.
    let mut out = Vec::with_capacity(arr.len());
    for v in arr.iter() {
        out.push(v.as_str()?.to_string());
    }
    Some(out)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn kp(segments: &[&str]) -> KeyPath {
        segments.iter().map(|s| s.to_string()).collect()
    }

    // -----------------------------------------------------------------------
    // JsonDoc
    // -----------------------------------------------------------------------

    mod json {
        use super::*;

        fn write_file(dir: &TempDir, name: &str, content: &str) -> std::path::PathBuf {
            let path = dir.path().join(name);
            std::fs::write(&path, content).unwrap();
            path
        }

        #[test]
        fn activate_preserves_foreign_content() {
            // Realistic seed: an npm-style package.json with user scripts,
            // engines, devDependencies — none of which rwv owns.
            let dir = TempDir::new().unwrap();
            let path = write_file(
                &dir,
                "package.json",
                r#"{
  "version": "1.2.3",
  "scripts": { "ci": "npm run build" },
  "engines": { "node": ">=18" }
}"#,
            );

            let owned = vec![
                (kp(&["private"]), Ownership::Author, OwnedValue::Bool(true)),
                (
                    kp(&["workspaces"]),
                    Ownership::Author,
                    OwnedValue::sorted_array(["github/acme/server", "github/acme/web"]),
                ),
            ];
            let result = merge_activate::<JsonDoc>(&path, &owned).unwrap();

            assert_eq!(result.authored.len(), 2);
            assert_eq!(result.deferred.len(), 0);

            let content = std::fs::read_to_string(&path).unwrap();
            let v: serde_json::Value = serde_json::from_str(&content).unwrap();
            // Owned keys present.
            assert_eq!(v["private"], serde_json::json!(true));
            assert_eq!(
                v["workspaces"],
                serde_json::json!(["github/acme/server", "github/acme/web"])
            );
            // Marker present (object form).
            assert_eq!(v["x-repoweave"]["managed"], serde_json::json!(true));
            // Foreign content survives.
            assert_eq!(v["version"], serde_json::json!("1.2.3"));
            assert_eq!(v["scripts"]["ci"], serde_json::json!("npm run build"));
            assert_eq!(v["engines"]["node"], serde_json::json!(">=18"));
        }

        #[test]
        fn activate_is_idempotent() {
            let dir = TempDir::new().unwrap();
            let path = write_file(
                &dir,
                "package.json",
                r#"{
  "version": "1.2.3",
  "scripts": { "ci": "npm run build" }
}"#,
            );

            let owned = vec![
                (kp(&["private"]), Ownership::Author, OwnedValue::Bool(true)),
                (
                    kp(&["workspaces"]),
                    Ownership::Author,
                    OwnedValue::sorted_array(["github/acme/server"]),
                ),
            ];
            merge_activate::<JsonDoc>(&path, &owned).unwrap();
            let first = std::fs::read_to_string(&path).unwrap();
            merge_activate::<JsonDoc>(&path, &owned).unwrap();
            let second = std::fs::read_to_string(&path).unwrap();
            assert_eq!(first, second, "second activate should be a no-op diff");
        }

        #[test]
        fn deactivate_strips_owned_and_preserves_foreign() {
            let dir = TempDir::new().unwrap();
            let path = write_file(
                &dir,
                "package.json",
                r#"{
  "x-repoweave": { "managed": true },
  "private": true,
  "workspaces": ["github/acme/server"],
  "scripts": { "ci": "npm test" },
  "version": "1.0.0"
}"#,
            );
            let owned_keys = vec![kp(&["private"]), kp(&["workspaces"])];
            strip_deactivate::<JsonDoc>(&path, &owned_keys).unwrap();
            assert!(
                path.exists(),
                "file should survive — foreign content present"
            );
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert!(v.get("private").is_none());
            assert!(v.get("workspaces").is_none());
            assert!(v.get("x-repoweave").is_none());
            assert_eq!(v["scripts"]["ci"], serde_json::json!("npm test"));
            assert_eq!(v["version"], serde_json::json!("1.0.0"));
        }

        #[test]
        fn deactivate_deletes_when_only_owned_present() {
            let dir = TempDir::new().unwrap();
            let path = write_file(
                &dir,
                "package.json",
                r#"{
  "x-repoweave": { "managed": true },
  "private": true,
  "workspaces": ["github/acme/server"]
}"#,
            );
            let owned_keys = vec![kp(&["private"]), kp(&["workspaces"])];
            strip_deactivate::<JsonDoc>(&path, &owned_keys).unwrap();
            assert!(!path.exists(), "file should be deleted (delete-if-empty)");
        }

        #[test]
        fn verify_and_warn_when_user_holds_pen() {
            // Owned key present but no marker → user took the pen.
            let dir = TempDir::new().unwrap();
            let path = write_file(
                &dir,
                "package.json",
                r#"{
  "private": true,
  "workspaces": ["packages/*"],
  "scripts": { "ci": "npm test" }
}"#,
            );
            let owned = vec![
                (kp(&["private"]), Ownership::Author, OwnedValue::Bool(true)),
                (
                    kp(&["workspaces"]),
                    Ownership::Author,
                    OwnedValue::sorted_array(["github/acme/server"]),
                ),
            ];
            let result = merge_activate::<JsonDoc>(&path, &owned).unwrap();
            // Both keys were present without a marker → both deferred.
            assert_eq!(result.deferred.len(), 2);
            assert!(result.authored.is_empty());
            // File unchanged in its owned-key values.
            let content = std::fs::read_to_string(&path).unwrap();
            let v: serde_json::Value = serde_json::from_str(&content).unwrap();
            assert_eq!(v["workspaces"], serde_json::json!(["packages/*"]));
            // No marker was added.
            assert!(v.get("x-repoweave").is_none());
        }

        #[test]
        fn strip_skips_when_marker_absent() {
            // User holds the pen on a hand-written package.json — strip is a
            // no-op (don't destroy what we don't own).
            let dir = TempDir::new().unwrap();
            let path = write_file(
                &dir,
                "package.json",
                r#"{
  "name": "my-app",
  "private": true,
  "workspaces": ["packages/*"]
}"#,
            );
            let owned_keys = vec![kp(&["private"]), kp(&["workspaces"])];
            strip_deactivate::<JsonDoc>(&path, &owned_keys).unwrap();
            // File untouched.
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(v["workspaces"], serde_json::json!(["packages/*"]));
            assert_eq!(v["private"], serde_json::json!(true));
        }

        #[test]
        fn object_form_sub_key_merge() {
            // npm object-form `workspaces` — rwv owns `workspaces.packages`,
            // preserves user's `workspaces.nohoist`.
            let dir = TempDir::new().unwrap();
            let path = write_file(
                &dir,
                "package.json",
                r#"{
  "x-repoweave": { "managed": true },
  "workspaces": {
    "packages": ["old/*"],
    "nohoist": ["**/react-native"]
  }
}"#,
            );
            let owned = vec![(
                kp(&["workspaces"]),
                Ownership::Author,
                OwnedValue::Object(BTreeMap::from([(
                    "packages".to_string(),
                    OwnedValue::sorted_array(["github/acme/web"]),
                )])),
            )];
            merge_activate::<JsonDoc>(&path, &owned).unwrap();
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(
                v["workspaces"]["packages"],
                serde_json::json!(["github/acme/web"])
            );
            assert_eq!(
                v["workspaces"]["nohoist"],
                serde_json::json!(["**/react-native"]),
                "nohoist must survive sub-key merge"
            );
        }

        #[test]
        fn vscode_marker_accepted_as_bool() {
            // RwvGeneratedMarker: existing on-disk shape is `rwv.generated: true`.
            // has_marker must recognize a plain bool true.
            let dir = TempDir::new().unwrap();
            let path = write_file(
                &dir,
                "foo.code-workspace",
                r#"{
  "rwv.generated": true,
  "folders": [{"path": ".", "name": "x (primary)"}]
}"#,
            );
            // strip_deactivate must NOT no-op — marker is present (as bool).
            let owned_keys = vec![kp(&["folders"])];
            strip_deactivate::<JsonDoc<RwvGeneratedMarker>>(&path, &owned_keys).unwrap();
            assert!(
                !path.exists(),
                "file should be deleted (was only owned-key content)"
            );
        }

        #[test]
        fn malformed_json_bails() {
            let dir = TempDir::new().unwrap();
            let path = write_file(&dir, "package.json", "{ not json");
            let owned = vec![(kp(&["private"]), Ownership::Author, OwnedValue::Bool(true))];
            let err = merge_activate::<JsonDoc>(&path, &owned).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.to_lowercase().contains("json") || msg.contains("package.json"),
                "error must name JSON or the file: {msg}"
            );
        }

        // --- Ownership::DefaultOnly -----------------------------------------

        #[test]
        fn default_only_sets_when_absent() {
            // When the key is absent, DefaultOnly should write the default value.
            let dir = TempDir::new().unwrap();
            let path = write_file(
                &dir,
                "package.json",
                r#"{
  "x-repoweave": { "managed": true },
  "workspaces": ["github/acme/server"]
}"#,
            );
            let owned = vec![
                (
                    kp(&["workspaces"]),
                    Ownership::Author,
                    OwnedValue::sorted_array(["github/acme/server"]),
                ),
                (
                    kp(&["description"]),
                    Ownership::DefaultOnly,
                    OwnedValue::String("default description".to_string()),
                ),
            ];
            merge_activate::<JsonDoc>(&path, &owned).unwrap();
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            // DefaultOnly key was absent → written with the default.
            assert_eq!(
                v["description"],
                serde_json::json!("default description"),
                "DefaultOnly key must be set when absent"
            );
        }

        #[test]
        fn default_only_does_not_overwrite_existing() {
            // When the key is present, DefaultOnly must leave the existing value alone.
            let dir = TempDir::new().unwrap();
            let path = write_file(
                &dir,
                "package.json",
                r#"{
  "x-repoweave": { "managed": true },
  "workspaces": ["github/acme/server"],
  "description": "user's own description"
}"#,
            );
            let owned = vec![
                (
                    kp(&["workspaces"]),
                    Ownership::Author,
                    OwnedValue::sorted_array(["github/acme/server"]),
                ),
                (
                    kp(&["description"]),
                    Ownership::DefaultOnly,
                    OwnedValue::String("default description".to_string()),
                ),
            ];
            merge_activate::<JsonDoc>(&path, &owned).unwrap();
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            // DefaultOnly key was present → existing value preserved.
            assert_eq!(
                v["description"],
                serde_json::json!("user's own description"),
                "DefaultOnly key must NOT overwrite an existing value"
            );
        }

        #[test]
        fn default_only_not_in_authored_or_deferred() {
            // DefaultOnly keys must not appear in MergeResult.authored or .deferred.
            let dir = TempDir::new().unwrap();
            let path = write_file(&dir, "package.json", r#"{}"#);
            let owned = vec![
                (
                    kp(&["workspaces"]),
                    Ownership::Author,
                    OwnedValue::sorted_array(["github/acme/server"]),
                ),
                (
                    kp(&["description"]),
                    Ownership::DefaultOnly,
                    OwnedValue::String("default".to_string()),
                ),
            ];
            let result = merge_activate::<JsonDoc>(&path, &owned).unwrap();
            assert_eq!(
                result.authored.len(),
                1,
                "only the Author key must appear in authored"
            );
            assert_eq!(result.authored[0], kp(&["workspaces"]));
            assert!(
                result.deferred.is_empty(),
                "DefaultOnly must not appear in deferred"
            );
        }

        #[test]
        fn default_only_not_stripped_on_deactivate() {
            // strip_deactivate only strips keys passed to it (which should be
            // Author keys). DefaultOnly keys are not passed to strip_deactivate
            // by the caller — they survive deactivation.
            let dir = TempDir::new().unwrap();
            let path = write_file(
                &dir,
                "package.json",
                r#"{
  "x-repoweave": { "managed": true },
  "workspaces": ["github/acme/server"],
  "description": "user's description"
}"#,
            );
            // Caller passes only Author keys to strip_deactivate.
            let owned_keys = vec![kp(&["workspaces"])];
            strip_deactivate::<JsonDoc>(&path, &owned_keys).unwrap();
            assert!(path.exists(), "file should survive — description present");
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert!(v.get("workspaces").is_none(), "Author key removed");
            assert_eq!(
                v["description"],
                serde_json::json!("user's description"),
                "DefaultOnly key must survive deactivate"
            );
        }
    }

    // -----------------------------------------------------------------------
    // TomlDoc
    // -----------------------------------------------------------------------

    mod toml {
        use super::*;

        fn write_file(dir: &TempDir, name: &str, content: &str) -> std::path::PathBuf {
            let path = dir.path().join(name);
            std::fs::write(&path, content).unwrap();
            path
        }

        const UV_SEED: &str = r#"[project]
name = "acme"
version = "0.1.0"

[tool.black]
line-length = 100
force-exclude = '''
^/vendor/
'''

[tool.ruff.lint]
select = ["E", "F"]
"#;

        #[test]
        fn activate_preserves_foreign_content_and_comments() {
            let dir = TempDir::new().unwrap();
            let path = write_file(&dir, "pyproject.toml", UV_SEED);

            let owned = vec![(
                kp(&["tool", "uv", "workspace", "members"]),
                Ownership::Author,
                OwnedValue::sorted_array(["github/acme/server", "github/acme/web"]),
            )];
            let result = merge_activate::<TomlDoc>(&path, &owned).unwrap();
            assert_eq!(result.authored.len(), 1);

            let text = std::fs::read_to_string(&path).unwrap();
            // Owned key set.
            assert!(text.contains(r#"members = ["github/acme/server", "github/acme/web"]"#));
            // Marker decor on the owned key.
            assert!(
                text.contains(TOML_MARKER_PREFIX),
                "marker decor must be present: {text}"
            );
            // Foreign content survives byte-stable substrings.
            assert!(text.contains(r#"name = "acme""#));
            assert!(text.contains(r#"line-length = 100"#));
            assert!(
                text.contains("^/vendor/"),
                "literal-string content must survive: {text}"
            );
            assert!(text.contains(r#"select = ["E", "F"]"#));
        }

        #[test]
        fn activate_is_idempotent() {
            let dir = TempDir::new().unwrap();
            let path = write_file(&dir, "pyproject.toml", UV_SEED);
            let owned = vec![(
                kp(&["tool", "uv", "workspace", "members"]),
                Ownership::Author,
                OwnedValue::sorted_array(["github/acme/server"]),
            )];
            merge_activate::<TomlDoc>(&path, &owned).unwrap();
            let first = std::fs::read_to_string(&path).unwrap();
            merge_activate::<TomlDoc>(&path, &owned).unwrap();
            let second = std::fs::read_to_string(&path).unwrap();
            assert_eq!(first, second);
        }

        #[test]
        fn deactivate_strips_owned_and_preserves_foreign() {
            let dir = TempDir::new().unwrap();
            // Pre-marked file as if rwv authored.
            let seed = format!(
                "{UV_SEED}\n[tool.uv.workspace]\n{TOML_MARKER_PREFIX}members = [\"github/acme/server\"]\n"
            );
            let path = write_file(&dir, "pyproject.toml", &seed);
            let owned_keys = vec![kp(&["tool", "uv", "workspace", "members"])];
            strip_deactivate::<TomlDoc>(&path, &owned_keys).unwrap();
            assert!(path.exists());
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(!text.contains("members ="), "members removed: {text}");
            assert!(!text.contains(TOML_MARKER_TEXT), "marker removed: {text}");
            // Foreign content survives.
            assert!(text.contains(r#"name = "acme""#));
            assert!(text.contains(r#"line-length = 100"#));
            assert!(text.contains("^/vendor/"));
        }

        #[test]
        fn deactivate_deletes_when_only_owned_present() {
            let dir = TempDir::new().unwrap();
            let seed = format!(
                "[tool.uv.workspace]\n{TOML_MARKER_PREFIX}members = [\"github/acme/server\"]\n"
            );
            let path = write_file(&dir, "pyproject.toml", &seed);
            let owned_keys = vec![kp(&["tool", "uv", "workspace", "members"])];
            strip_deactivate::<TomlDoc>(&path, &owned_keys).unwrap();
            assert!(!path.exists(), "file should be deleted (delete-if-empty)");
        }

        #[test]
        fn verify_and_warn_when_user_holds_pen() {
            // members present (as a native glob) but no marker.
            let dir = TempDir::new().unwrap();
            let seed = format!("{UV_SEED}\n[tool.uv.workspace]\nmembers = [\"packages/*\"]\n");
            let path = write_file(&dir, "pyproject.toml", &seed);
            let owned = vec![(
                kp(&["tool", "uv", "workspace", "members"]),
                Ownership::Author,
                OwnedValue::sorted_array(["github/acme/server"]),
            )];
            let result = merge_activate::<TomlDoc>(&path, &owned).unwrap();
            assert_eq!(result.deferred.len(), 1);
            assert!(result.authored.is_empty());
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(
                text.contains(r#"members = ["packages/*"]"#),
                "user's members must be untouched: {text}"
            );
            assert!(
                !text.contains(TOML_MARKER_TEXT),
                "marker must not be applied: {text}"
            );
        }

        #[test]
        fn strip_skips_when_marker_absent() {
            let dir = TempDir::new().unwrap();
            let seed = format!("{UV_SEED}\n[tool.uv.workspace]\nmembers = [\"packages/*\"]\n");
            let path = write_file(&dir, "pyproject.toml", &seed);
            let owned_keys = vec![kp(&["tool", "uv", "workspace", "members"])];
            strip_deactivate::<TomlDoc>(&path, &owned_keys).unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(
                text.contains(r#"members = ["packages/*"]"#),
                "hand-owned members must survive: {text}"
            );
        }

        #[test]
        fn malformed_toml_bails() {
            let dir = TempDir::new().unwrap();
            let path = write_file(&dir, "pyproject.toml", "[ not toml");
            let owned = vec![(kp(&["x", "y"]), Ownership::Author, OwnedValue::Bool(true))];
            let err = merge_activate::<TomlDoc>(&path, &owned).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.to_lowercase().contains("toml") || msg.contains("pyproject.toml"),
                "error must name TOML or the file: {msg}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // YamlDoc
    // -----------------------------------------------------------------------

    mod yaml {
        use super::*;

        fn write_file(dir: &TempDir, name: &str, content: &str) -> std::path::PathBuf {
            let path = dir.path().join(name);
            std::fs::write(&path, content).unwrap();
            path
        }

        const PNPM_SEED: &str = r#"# shared dependency versions
catalog:
  react: ^18.0.0
  react-dom: ^18.0.0

overrides:
  lodash@<4.17.21: '>=4.17.21'
"#;

        #[test]
        fn activate_preserves_foreign_content() {
            let dir = TempDir::new().unwrap();
            let path = write_file(&dir, "pnpm-workspace.yaml", PNPM_SEED);

            let owned = vec![(
                kp(&["packages"]),
                Ownership::Author,
                OwnedValue::sorted_array(["github/acme/server"]),
            )];
            let result = merge_activate::<YamlDoc>(&path, &owned).unwrap();
            assert_eq!(result.authored.len(), 1);

            let text = std::fs::read_to_string(&path).unwrap();
            assert!(
                text.contains("# shared dependency versions"),
                "comment must survive: {text}"
            );
            assert!(text.contains("catalog:"));
            assert!(text.contains("react: ^18.0.0"));
            assert!(text.contains("overrides:"));
            assert!(
                text.contains(YAML_MARKER_LINE),
                "marker line must be present: {text}"
            );
            assert!(text.contains("- github/acme/server"));
        }

        #[test]
        fn activate_is_idempotent() {
            let dir = TempDir::new().unwrap();
            let path = write_file(&dir, "pnpm-workspace.yaml", PNPM_SEED);
            let owned = vec![(
                kp(&["packages"]),
                Ownership::Author,
                OwnedValue::sorted_array(["github/acme/server"]),
            )];
            merge_activate::<YamlDoc>(&path, &owned).unwrap();
            let first = std::fs::read_to_string(&path).unwrap();
            merge_activate::<YamlDoc>(&path, &owned).unwrap();
            let second = std::fs::read_to_string(&path).unwrap();
            assert_eq!(first, second);
        }

        #[test]
        fn deactivate_strips_owned_and_preserves_foreign() {
            let dir = TempDir::new().unwrap();
            let seed =
                format!("{PNPM_SEED}\n{YAML_MARKER_LINE}\npackages:\n  - github/acme/server\n");
            let path = write_file(&dir, "pnpm-workspace.yaml", &seed);
            let owned_keys = vec![kp(&["packages"])];
            strip_deactivate::<YamlDoc>(&path, &owned_keys).unwrap();
            assert!(path.exists());
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(!text.contains(YAML_MARKER_LINE));
            assert!(!text.contains("- github/acme/server"));
            assert!(text.contains("catalog:"));
            assert!(text.contains("react: ^18.0.0"));
            assert!(text.contains("overrides:"));
        }

        #[test]
        fn deactivate_deletes_when_only_owned_present() {
            let dir = TempDir::new().unwrap();
            let seed = format!(
                "{YAML_MARKER_LINE}\npackages:\n  - github/acme/server\n  - github/acme/web\n"
            );
            let path = write_file(&dir, "pnpm-workspace.yaml", &seed);
            let owned_keys = vec![kp(&["packages"])];
            strip_deactivate::<YamlDoc>(&path, &owned_keys).unwrap();
            assert!(!path.exists(), "file should be deleted (delete-if-empty)");
        }

        #[test]
        fn verify_and_warn_when_user_holds_pen() {
            // packages: present with no marker — user took the pen.
            let seed = "packages:\n  - packages/*\n";
            let dir = TempDir::new().unwrap();
            let path = write_file(&dir, "pnpm-workspace.yaml", seed);
            let owned = vec![(
                kp(&["packages"]),
                Ownership::Author,
                OwnedValue::sorted_array(["github/acme/server"]),
            )];
            let result = merge_activate::<YamlDoc>(&path, &owned).unwrap();
            assert_eq!(result.deferred.len(), 1);
            assert!(result.authored.is_empty());
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(
                text.contains("- packages/*"),
                "user's packages must be untouched: {text}"
            );
            assert!(
                !text.contains(YAML_MARKER_LINE),
                "marker must not appear: {text}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // GoWorkDoc
    // -----------------------------------------------------------------------

    mod go {
        use super::*;

        fn write_file(dir: &TempDir, name: &str, content: &str) -> std::path::PathBuf {
            let path = dir.path().join(name);
            std::fs::write(&path, content).unwrap();
            path
        }

        const GO_SEED: &str = r#"go 1.26

toolchain go1.26.0

godebug default=go1.26

use (
	./old
)

replace example.com/legacy => ./vendor/legacy
"#;

        #[test]
        fn activate_preserves_foreign_content() {
            let dir = TempDir::new().unwrap();
            let path = write_file(&dir, "go.work", GO_SEED);
            // Seed has a `use` block but no marker → user took the pen.
            // Use a fresh seed with no use-block to test author-from-empty.
            let fresh_seed =
                "go 1.26\n\ntoolchain go1.26.0\n\nreplace example.com/legacy => ./vendor/legacy\n";
            std::fs::write(&path, fresh_seed).unwrap();

            let owned = vec![(
                kp(&["use"]),
                Ownership::Author,
                OwnedValue::sorted_array(["./repoweave", "./some-go-tool"]),
            )];
            let result = merge_activate::<GoWorkDoc>(&path, &owned).unwrap();
            assert_eq!(result.authored.len(), 1);

            let text = std::fs::read_to_string(&path).unwrap();
            assert!(text.contains("go 1.26"), "go line preserved: {text}");
            assert!(text.contains("toolchain go1.26.0"));
            assert!(text.contains("replace example.com/legacy"));
            assert!(text.contains(GO_MARKER_LINE), "marker present: {text}");
            assert!(text.contains("./repoweave"));
            assert!(text.contains("./some-go-tool"));
        }

        #[test]
        fn activate_is_idempotent() {
            let dir = TempDir::new().unwrap();
            let path = write_file(
                &dir,
                "go.work",
                "go 1.26\n\nreplace example.com/legacy => ./vendor/legacy\n",
            );
            let owned = vec![(
                kp(&["use"]),
                Ownership::Author,
                OwnedValue::sorted_array(["./repoweave"]),
            )];
            merge_activate::<GoWorkDoc>(&path, &owned).unwrap();
            let first = std::fs::read_to_string(&path).unwrap();
            merge_activate::<GoWorkDoc>(&path, &owned).unwrap();
            let second = std::fs::read_to_string(&path).unwrap();
            assert_eq!(first, second);
        }

        #[test]
        fn deactivate_strips_use_and_preserves_replace_toolchain() {
            let dir = TempDir::new().unwrap();
            let seed = format!(
                "go 1.26\n\ntoolchain go1.26.0\n\n{GO_MARKER_LINE}\nuse (\n\t./repoweave\n)\n\nreplace example.com/legacy => ./vendor/legacy\n"
            );
            let path = write_file(&dir, "go.work", &seed);
            let owned_keys = vec![kp(&["use"])];
            strip_deactivate::<GoWorkDoc>(&path, &owned_keys).unwrap();
            assert!(path.exists());
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(!text.contains(GO_MARKER_LINE));
            assert!(!text.contains("./repoweave"));
            assert!(text.contains("go 1.26"), "go line preserved: {text}");
            assert!(text.contains("toolchain go1.26.0"));
            assert!(text.contains("replace example.com/legacy"));
        }

        #[test]
        fn deactivate_deletes_when_only_owned_present() {
            let dir = TempDir::new().unwrap();
            // Only the marker and use block — no other content. is_empty for
            // GoWorkDoc treats comments as foreign, so we strip and the
            // remaining text must be all-comment-or-blank for delete.
            let seed = format!("{GO_MARKER_LINE}\nuse (\n\t./repoweave\n)\n");
            let path = write_file(&dir, "go.work", &seed);
            let owned_keys = vec![kp(&["use"])];
            strip_deactivate::<GoWorkDoc>(&path, &owned_keys).unwrap();
            assert!(!path.exists(), "file should be deleted (delete-if-empty)");
        }

        #[test]
        fn verify_and_warn_when_user_holds_pen() {
            // `use (...)` present but no marker — user took the pen.
            let dir = TempDir::new().unwrap();
            let path = write_file(&dir, "go.work", "go 1.26\n\nuse (\n\t./mine\n)\n");
            let owned = vec![(
                kp(&["use"]),
                Ownership::Author,
                OwnedValue::sorted_array(["./repoweave"]),
            )];
            let result = merge_activate::<GoWorkDoc>(&path, &owned).unwrap();
            assert_eq!(result.deferred.len(), 1);
            assert!(result.authored.is_empty());
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(
                text.contains("./mine"),
                "user's use block must be untouched: {text}"
            );
            assert!(!text.contains("./repoweave"));
            assert!(!text.contains(GO_MARKER_LINE));
        }
    }
}
