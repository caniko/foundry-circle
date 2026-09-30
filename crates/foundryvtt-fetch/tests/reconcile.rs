#![cfg(unix)]

use std::{fs, path::Path};

use foundryvtt_fetch::reconcile::{DesiredManifest, DesiredPackage, ReconcileError, reconcile};

fn package(root: &Path, id: &str) -> DesiredPackage {
    let store_path = root.join(format!("store-{id}"));
    fs::create_dir(&store_path).unwrap();
    DesiredPackage {
        kind: "module".to_owned(),
        id: id.to_owned(),
        state: "present".to_owned(),
        version: "1.0.0".to_owned(),
        store_path,
    }
}

fn manifest(packages: Vec<DesiredPackage>) -> DesiredManifest {
    DesiredManifest {
        schema_version: 1,
        packages,
    }
}

#[test]
fn foreign_collision_does_not_partially_install_or_write_state() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let state = root.path().join("state.json");
    let first = package(root.path(), "a-managed");
    let second = package(root.path(), "z-foreign");
    let foreign = data.join("Data/modules/z-foreign");
    fs::create_dir_all(&foreign).unwrap();
    fs::write(foreign.join("operator.txt"), "preserve me").unwrap();

    let result = reconcile(manifest(vec![first, second]), &data, &state);

    assert!(matches!(result, Err(ReconcileError::ForeignPath(path)) if path == foreign));
    assert!(!data.join("Data/modules/a-managed").exists());
    assert!(!state.exists());
    assert_eq!(
        fs::read_to_string(foreign.join("operator.txt")).unwrap(),
        "preserve me"
    );
}

#[test]
fn only_explicit_tombstones_remove_previously_managed_links() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let state = root.path().join("state.json");
    let retained = package(root.path(), "retained");
    let removed = package(root.path(), "removed");
    reconcile(
        manifest(vec![retained.clone(), removed.clone()]),
        &data,
        &state,
    )
    .unwrap();

    let mut tombstone = removed;
    tombstone.state = "absent".to_owned();
    reconcile(manifest(vec![tombstone]), &data, &state).unwrap();

    assert_eq!(
        fs::read_link(data.join("Data/modules/retained")).unwrap(),
        retained.store_path
    );
    assert!(
        data.join("Data/modules/removed")
            .symlink_metadata()
            .is_err()
    );
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    assert!(saved["packages"].get("module/retained").is_some());
    assert!(saved["packages"].get("module/removed").is_none());
}

#[test]
fn tombstone_refuses_a_tampered_link_and_preserves_state() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let state = root.path().join("state.json");
    let mut managed = package(root.path(), "managed");
    let foreign = package(root.path(), "foreign");
    reconcile(manifest(vec![managed.clone()]), &data, &state).unwrap();
    let saved = fs::read(&state).unwrap();
    let target = data.join("Data/modules/managed");
    fs::remove_file(&target).unwrap();
    std::os::unix::fs::symlink(&foreign.store_path, &target).unwrap();
    managed.state = "absent".to_owned();

    let result = reconcile(manifest(vec![managed]), &data, &state);

    assert!(matches!(result, Err(ReconcileError::StateMismatch(path)) if path == target));
    assert_eq!(fs::read_link(&target).unwrap(), foreign.store_path);
    assert_eq!(fs::read(&state).unwrap(), saved);
}
