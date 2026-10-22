//! Locations reached through a symlink compare as the directory they name.
//!
//! macOS hands every temporary directory out under `/var`, a symlink to
//! `/private/var`, so a record written in one spelling and a root resolved in
//! the other are one directory under two strings. Here an explicit symlink
//! puts the same geometry under Linux: `real/` holds the weave, and `link/`
//! names it.
//!
//! Each record below is written in the `link/` spelling, by hand where an
//! earlier rwv would have written it, and read back by code holding the
//! `real/` spelling — or the reverse. Abort's savepoint namespace under a
//! symlinked op record is pinned beside the rest of abort's cells, in
//! `tests/crash_matrix_test.rs`.

mod common;

use repoweave::manifest::{ProjectName, WorkweaveName};
use repoweave::path_spelling::CanonicalPath;
use repoweave::workspace::WorkweaveMarker;
use std::path::{Path, PathBuf};

const PROJECT: &str = "web-app";

struct Geometry {
    _tmp: tempfile::TempDir,
    real: PathBuf,
    link: PathBuf,
}

/// `real/ws/projects/web-app`, and `link` → `real`.
fn geometry() -> Geometry {
    let tmp = common::tempdir().unwrap();
    let real = tmp.path().join("real");
    std::fs::create_dir_all(real.join("ws").join("projects").join(PROJECT)).unwrap();
    let link = tmp.path().join("link");
    repoweave::symlink::create(&real, &link, repoweave::symlink::LinkTarget::Directory).unwrap();
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "fixture precondition: `link` must be a symlink, or no spelling differs"
    );
    Geometry {
        _tmp: tmp,
        real,
        link,
    }
}

fn project() -> ProjectName {
    ProjectName::new(PROJECT).unwrap()
}

/// A workweave of `primary`'s at `dir`, its marker written in whatever
/// spelling `primary` is given in.
fn workweave_at(primary: &Path, dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    WorkweaveMarker::new(primary, project(), primary)
        .write(dir)
        .unwrap();
}

#[test]
fn an_index_written_in_a_symlinked_spelling_reads_as_the_directories_it_names() {
    let g = geometry();
    let container = g.real.join("workweaves");
    std::fs::create_dir_all(container.join("web-app--feat")).unwrap();
    let index = format!(
        "{{\"container\": \"{}\", \"workweaves\": {{\"feat\": \"{}\"}}, \"receipts\": []}}\n",
        common::json_escaped(g.link.join("workweaves")),
        common::json_escaped(g.link.join("workweaves").join("web-app--feat")),
    );
    std::fs::write(
        g.real
            .join("ws")
            .join("projects")
            .join(PROJECT)
            .join(".rwv-workweave-index"),
        index,
    )
    .unwrap();

    let read = repoweave::workweave_index::read(&g.real.join("ws"), &project())
        .unwrap()
        .expect("the planted index");
    assert_eq!(read.container, CanonicalPath::of(&container));
    assert_eq!(
        read.workweaves.get(&WorkweaveName::new("feat").unwrap()),
        Some(&CanonicalPath::of(&container.join("web-app--feat")))
    );
    assert_eq!(
        read.container.as_path(),
        repoweave::path_spelling::resolve(&container).unwrap(),
        "the recorded container is the resolved spelling, not the one written"
    );
}

#[test]
fn a_marker_built_from_a_symlinked_primary_names_the_resolved_one() {
    let g = geometry();
    let ww = g.real.join("web-app--feat");
    workweave_at(&g.link.join("ws"), &ww);

    let marker = WorkweaveMarker::read(&ww).unwrap().expect("the marker");
    assert!(marker.names_primary(&CanonicalPath::of(&g.real.join("ws"))));
    assert_eq!(
        marker.primary().as_path(),
        repoweave::path_spelling::resolve(&g.real.join("ws")).unwrap()
    );
}

#[test]
fn a_marker_written_in_a_symlinked_spelling_reads_as_the_primary_it_names() {
    let g = geometry();
    let ww = g.real.join("web-app--feat");
    std::fs::create_dir_all(&ww).unwrap();
    let primary = g.link.join("ws");
    std::fs::write(
        ww.join(".rwv-workweave"),
        format!(
            "{{\"primary\": \"{p}\", \"project\": \"{PROJECT}\", \"parent\": \"{p}\"}}\n",
            p = common::json_escaped(&primary),
        ),
    )
    .unwrap();

    let marker = WorkweaveMarker::read(&ww).unwrap().expect("the marker");
    assert!(marker.names_primary(&CanonicalPath::of(&g.real.join("ws"))));
    assert_eq!(marker.parent(), marker.primary());
}

/// The scan is handed the primary in the spelling its caller holds, and the
/// workweave it finds recorded the other one.
#[test]
fn a_container_scan_from_a_symlinked_primary_finds_the_workweave() {
    let g = geometry();
    let container = g.real.join("workweaves");
    let ww = container.join("web-app--feat");
    workweave_at(&g.real.join("ws"), &ww);

    let found = repoweave::workweave::doctor_scan_container(&g.link.join("ws"), &container);
    assert_eq!(
        found
            .iter()
            .map(|(id, _)| (id.project.as_str(), id.name.as_str()))
            .collect::<Vec<_>>(),
        vec![(PROJECT, "feat")],
        "the scan must recognize a marker naming this primary in either spelling"
    );
}

#[test]
fn a_registered_workweave_resolves_through_a_symlinked_primary() {
    let g = geometry();
    let ww = g.real.join("workweaves").join("web-app--feat");
    workweave_at(&g.real.join("ws"), &ww);
    repoweave::workweave_index::record_workweave(
        &g.real.join("ws"),
        &project(),
        &WorkweaveName::new("feat").unwrap(),
        CanonicalPath::of(&g.link.join("workweaves").join("web-app--feat")),
    )
    .unwrap();

    let found = repoweave::workweave::resolve_registered_workweave(
        &g.link.join("ws"),
        &project(),
        &WorkweaveName::new("feat").unwrap(),
    )
    .unwrap();
    assert_eq!(
        found.map(|p| CanonicalPath::of(&p)),
        Some(CanonicalPath::of(&ww))
    );
}
