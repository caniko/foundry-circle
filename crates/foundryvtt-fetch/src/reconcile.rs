//! Fail-closed initial seeding of mutable Foundry module/system directories.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io,
    os::unix::fs::{self as unix_fs, PermissionsExt},
    path::{Path, PathBuf},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("desired package manifest is invalid: {0}")]
    Manifest(#[from] serde_json::Error),
    #[error("managed package state is invalid: {0}")]
    State(#[source] serde_json::Error),
    #[error("package has an unsafe id: {0}")]
    UnsafeId(String),
    #[error("package store path is not an absolute directory: {0}")]
    InvalidStorePath(String),
    #[error("package target is not a directory: {0}")]
    InvalidTarget(PathBuf),
    #[error("reconciliation requires Unix symlink support")]
    UnsupportedPlatform,
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DesiredPackage {
    pub kind: String,
    pub id: String,
    #[serde(default = "default_state")]
    pub state: String,
    pub version: String,
    pub store_path: PathBuf,
}

fn default_state() -> String {
    "present".to_owned()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesiredManifest {
    pub schema_version: u32,
    pub packages: Vec<DesiredPackage>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ManagedState {
    schema_version: u32,
    packages: BTreeMap<String, ManagedPackage>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ManagedPackage {
    kind: String,
    id: String,
    version: String,
    store_path: PathBuf,
}

pub fn reconcile_files(
    desired_path: &Path,
    data_dir: &Path,
    state_path: &Path,
) -> Result<(), ReconcileError> {
    let desired: DesiredManifest = serde_json::from_slice(&fs::read(desired_path)?)?;
    reconcile(desired, data_dir, state_path)
}

/// Seed only missing package directories below `Data/modules` and
/// `Data/systems`. The copied contents belong to Foundry and may be edited in
/// place. The state file records that initialization happened once; later
/// manifest changes, omissions, and target deletion do not reseed or remove
/// the directory. `state = "absent"` explicitly clears that initialization
/// marker without touching the target.
pub fn reconcile(
    desired: DesiredManifest,
    data_dir: &Path,
    state_path: &Path,
) -> Result<(), ReconcileError> {
    if desired.schema_version != SCHEMA_VERSION {
        return Err(ReconcileError::UnsafeId(format!(
            "unsupported desired schema {}",
            desired.schema_version
        )));
    }
    let lock_path = state_path.with_extension("lock");
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock = File::create(&lock_path)?;
    lock.lock_exclusive()?;
    let previous = read_state(state_path)?;
    let result = reconcile_locked(&desired, &previous, data_dir, state_path);
    drop(lock);
    result
}

fn reconcile_locked(
    desired: &DesiredManifest,
    previous: &ManagedState,
    data_dir: &Path,
    state_path: &Path,
) -> Result<(), ReconcileError> {
    let mut declared = BTreeMap::new();
    let mut present = BTreeMap::new();
    let mut absent = BTreeMap::new();
    for package in &desired.packages {
        validate_package(package)?;
        let key = key(&package.kind, &package.id);
        if declared.insert(key.clone(), package).is_some() {
            return Err(ReconcileError::UnsafeId(format!("duplicate package {key}")));
        }
        if package.state == "present" {
            present.insert(key, package);
        } else {
            absent.insert(key, package);
        }
    }

    // Preflight all targets and any legacy links before copying one package.
    // This keeps a bad target from leaving a partially initialized set.
    for package in present.values() {
        let path = target(data_dir, package);
        if let Some(previous_package) = previous.packages.get(&key(&package.kind, &package.id)) {
            check_initialized_target(&path, &previous_package.store_path)?;
        } else {
            check_seed_target(&path, &package.store_path)?;
        }
    }

    let mut packages = previous.packages.clone();
    for package in present.values() {
        let name = key(&package.kind, &package.id);
        let path = target(data_dir, package);
        if let Some(previous_package) = previous.packages.get(&name) {
            materialize_legacy_link(&path, &previous_package.store_path)?;
        } else {
            seed_if_missing(&path, &package.store_path)?;
            packages.insert(
                name,
                ManagedPackage {
                    kind: package.kind.clone(),
                    id: package.id.clone(),
                    version: package.version.clone(),
                    store_path: package.store_path.clone(),
                },
            );
        }
    }
    // Undeclared packages are intentionally preserved. An explicit absent
    // entry resets only the ledger, so a later present entry may initialize a
    // missing target again.
    for name in absent.keys() {
        packages.remove(name);
    }

    let state = ManagedState {
        schema_version: SCHEMA_VERSION,
        packages,
    };
    write_state(state_path, &state)
}

fn read_state(path: &Path) -> Result<ManagedState, ReconcileError> {
    if !path.is_file() {
        return Ok(ManagedState {
            schema_version: SCHEMA_VERSION,
            packages: BTreeMap::new(),
        });
    }
    let state: ManagedState =
        serde_json::from_slice(&fs::read(path)?).map_err(ReconcileError::State)?;
    if state.schema_version != SCHEMA_VERSION {
        return Err(ReconcileError::UnsafeId(format!(
            "unsupported managed state schema {}",
            state.schema_version
        )));
    }
    Ok(state)
}

fn validate_package(package: &DesiredPackage) -> Result<(), ReconcileError> {
    if package.kind != "module" && package.kind != "system" {
        return Err(ReconcileError::UnsafeId(package.kind.clone()));
    }
    if package.state != "present" && package.state != "absent" {
        return Err(ReconcileError::UnsafeId(format!(
            "{} has invalid state {}",
            package.id, package.state
        )));
    }
    if package.id.is_empty()
        || package.id == "."
        || package.id == ".."
        || !package
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(ReconcileError::UnsafeId(package.id.clone()));
    }
    if package.state == "absent" {
        return Ok(());
    }
    if package.version.trim().is_empty() {
        return Err(ReconcileError::UnsafeId(format!(
            "{} has no version",
            package.id
        )));
    }
    validate_store_path(&package.store_path)
}

fn validate_store_path(path: &Path) -> Result<(), ReconcileError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| ReconcileError::InvalidStorePath(path.display().to_string()))?;
    if !metadata.file_type().is_dir() || path.is_relative() {
        return Err(ReconcileError::InvalidStorePath(path.display().to_string()));
    }
    Ok(())
}

fn key(kind: &str, id: &str) -> String {
    format!("{kind}/{id}")
}

fn target(data: &Path, package: &impl PackageLike) -> PathBuf {
    data.join("Data")
        .join(if package.kind() == "module" {
            "modules"
        } else {
            "systems"
        })
        .join(package.id())
}

trait PackageLike {
    fn kind(&self) -> &str;
    fn id(&self) -> &str;
}

impl PackageLike for DesiredPackage {
    fn kind(&self) -> &str {
        &self.kind
    }

    fn id(&self) -> &str {
        &self.id
    }
}

impl<T: PackageLike + ?Sized> PackageLike for &T {
    fn kind(&self) -> &str {
        (*self).kind()
    }

    fn id(&self) -> &str {
        (*self).id()
    }
}

fn check_seed_target(path: &Path, source: &Path) -> Result<(), ReconcileError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() && fs::read_link(path)? == source {
        validate_store_path(source)?;
    } else {
        validate_existing_target(path)?;
    }
    Ok(())
}

fn check_initialized_target(path: &Path, source: &Path) -> Result<(), ReconcileError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() && fs::read_link(path)? == source {
        validate_store_path(source)?;
    } else {
        validate_existing_target(path)?;
    }
    Ok(())
}

fn validate_existing_target(path: &Path) -> Result<(), ReconcileError> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) | Err(_) => Err(ReconcileError::InvalidTarget(path.to_path_buf())),
    }
}

fn seed_if_missing(target: &Path, source: &Path) -> Result<(), ReconcileError> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_symlink() && fs::read_link(target)? == source => {
            materialize_legacy_link(target, source)?;
            return Ok(());
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let parent = target
        .parent()
        .ok_or_else(|| ReconcileError::InvalidTarget(target.to_path_buf()))?;
    fs::create_dir_all(parent)?;
    let temp = tempfile::tempdir_in(parent)?;
    copy_tree(source, temp.path(), source)?;
    // The reconciliation lock serializes normal writers. The preflight above
    // also ensures we never intentionally replace an existing target.
    if fs::symlink_metadata(target).is_ok() {
        return Ok(());
    }
    fs::rename(temp.path(), target)?;
    Ok(())
}

fn materialize_legacy_link(target: &Path, source: &Path) -> Result<(), ReconcileError> {
    let metadata = match fs::symlink_metadata(target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_symlink() || fs::read_link(target)? != source {
        return Ok(());
    }
    let parent = target
        .parent()
        .ok_or_else(|| ReconcileError::InvalidTarget(target.to_path_buf()))?;
    let temp = tempfile::tempdir_in(parent)?;
    copy_tree(source, temp.path(), source)?;

    let displaced = NamedTempFile::new_in(parent)?;
    let displaced_path = displaced.into_temp_path();
    fs::remove_file(&displaced_path)?;
    fs::rename(target, &displaced_path)?;
    if let Err(error) = fs::rename(temp.path(), target) {
        let _ = fs::rename(&displaced_path, target);
        return Err(error.into());
    }
    fs::remove_file(&displaced_path)?;
    Ok(())
}

fn copy_tree(source_root: &Path, target_root: &Path, source: &Path) -> Result<(), ReconcileError> {
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.file_type().is_dir() {
        return Err(ReconcileError::InvalidStorePath(
            source.display().to_string(),
        ));
    }
    fs::create_dir_all(target_root)?;
    make_mutable(target_root, metadata.permissions().mode())?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target_root.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.file_type().is_dir() {
            copy_tree(source_root, &target_path, &source_path)?;
        } else if metadata.file_type().is_file() {
            fs::copy(&source_path, &target_path)?;
            make_mutable(&target_path, metadata.permissions().mode())?;
        } else if metadata.file_type().is_symlink() {
            let relative = source_path
                .strip_prefix(source_root)
                .map_err(|_| ReconcileError::InvalidStorePath(source_root.display().to_string()))?;
            let name = relative.to_str().ok_or_else(|| {
                ReconcileError::InvalidStorePath(source_root.display().to_string())
            })?;
            let link = fs::read_link(&source_path)?;
            let link_target = link.to_str().ok_or_else(|| {
                ReconcileError::InvalidStorePath(source_root.display().to_string())
            })?;
            if !crate::safe_symlink_target(name, link_target) {
                return Err(ReconcileError::InvalidStorePath(
                    source_root.display().to_string(),
                ));
            }
            unix_fs::symlink(&link, &target_path).map_err(map_symlink_error)?;
        } else {
            return Err(ReconcileError::InvalidStorePath(
                source_root.display().to_string(),
            ));
        }
    }
    Ok(())
}

fn make_mutable(path: &Path, mode: u32) -> Result<(), ReconcileError> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o200))?;
    Ok(())
}

fn map_symlink_error(error: io::Error) -> ReconcileError {
    if error.kind() == io::ErrorKind::Unsupported {
        ReconcileError::UnsupportedPlatform
    } else {
        ReconcileError::Io(error)
    }
}

fn write_state(path: &Path, state: &ManagedState) -> Result<(), ReconcileError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut temp = NamedTempFile::new_in(path.parent().unwrap_or_else(|| Path::new(".")))?;
    serde_json::to_writer_pretty(temp.as_file_mut(), state)?;
    temp.as_file().sync_all()?;
    fs::rename(temp.into_temp_path(), path)?;
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(root: &Path, kind: &str, id: &str, version: &str) -> DesiredPackage {
        DesiredPackage {
            kind: kind.into(),
            id: id.into(),
            state: "present".into(),
            version: version.into(),
            store_path: root.join(version),
        }
    }

    fn manifest(packages: Vec<DesiredPackage>) -> DesiredManifest {
        DesiredManifest {
            schema_version: SCHEMA_VERSION,
            packages,
        }
    }

    #[test]
    fn seeds_once_and_preserves_manual_changes() {
        let temp = tempfile::tempdir().unwrap();
        let source_one = temp.path().join("source-one");
        let source_two = temp.path().join("source-two");
        fs::create_dir_all(source_one.join("nested")).unwrap();
        fs::write(source_one.join("content.txt"), b"v1").unwrap();
        fs::write(source_one.join("nested/extra.txt"), b"extra").unwrap();
        fs::create_dir_all(&source_two).unwrap();
        fs::write(source_two.join("content.txt"), b"v2").unwrap();
        let data = temp.path().join("data");
        let state = temp.path().join("state.json");
        let first = DesiredPackage {
            store_path: source_one.clone(),
            ..p(temp.path(), "module", "demo", "1")
        };
        let second = DesiredPackage {
            store_path: source_two,
            version: "2".into(),
            ..p(temp.path(), "module", "demo", "2")
        };
        let target = data.join("Data/modules/demo");

        reconcile(manifest(vec![first]), &data, &state).unwrap();
        assert!(
            !fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(target.join("content.txt")).unwrap(), b"v1");
        assert!(
            fs::metadata(target.join("content.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o200
                != 0
        );

        fs::write(target.join("content.txt"), b"manual").unwrap();
        reconcile(manifest(vec![second]), &data, &state).unwrap();
        assert_eq!(fs::read(target.join("content.txt")).unwrap(), b"manual");

        reconcile(manifest(vec![]), &data, &state).unwrap();
        assert_eq!(fs::read(target.join("content.txt")).unwrap(), b"manual");
    }

    #[test]
    fn accepts_existing_directories_and_absent_only_resets_the_marker() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("content.txt"), b"seed").unwrap();
        let data = temp.path().join("data");
        let state = temp.path().join("state.json");
        let package = DesiredPackage {
            store_path: source.clone(),
            ..p(temp.path(), "system", "rules", "1")
        };
        let target = data.join("Data/systems/rules");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("content.txt"), b"manual").unwrap();

        reconcile(manifest(vec![package.clone()]), &data, &state).unwrap();
        assert_eq!(fs::read(target.join("content.txt")).unwrap(), b"manual");
        reconcile(
            manifest(vec![DesiredPackage {
                state: "absent".into(),
                version: String::new(),
                store_path: PathBuf::new(),
                ..package.clone()
            }]),
            &data,
            &state,
        )
        .unwrap();
        assert!(fs::metadata(&target).unwrap().is_dir());
        fs::remove_dir_all(&target).unwrap();
        reconcile(manifest(vec![package]), &data, &state).unwrap();
        assert_eq!(fs::read(target.join("content.txt")).unwrap(), b"seed");
    }

    #[test]
    fn materializes_legacy_recorded_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("content.txt"), b"seed").unwrap();
        let data = temp.path().join("data");
        let state = temp.path().join("state.json");
        let package = DesiredPackage {
            store_path: source.clone(),
            ..p(temp.path(), "module", "demo", "1")
        };
        let target = data.join("Data/modules/demo");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        unix_fs::symlink(&source, &target).unwrap();
        fs::create_dir_all(state.parent().unwrap_or_else(|| Path::new("."))).unwrap();
        fs::write(
            &state,
            serde_json::to_vec(&ManagedState {
                schema_version: SCHEMA_VERSION,
                packages: BTreeMap::from([(
                    "module/demo".into(),
                    ManagedPackage {
                        kind: "module".into(),
                        id: "demo".into(),
                        version: "1".into(),
                        store_path: source.clone(),
                    },
                )]),
            })
            .unwrap(),
        )
        .unwrap();

        reconcile(manifest(vec![package]), &data, &state).unwrap();
        assert!(
            !fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(target.join("content.txt")).unwrap(), b"seed");
        fs::write(target.join("content.txt"), b"manual").unwrap();
        assert_eq!(fs::read(source.join("content.txt")).unwrap(), b"seed");
    }

    #[test]
    fn rejects_invalid_ids_and_duplicate_packages() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir_all(&source).unwrap();
        let data = temp.path().join("data");
        let state = temp.path().join("state.json");
        let mut unsafe_package = p(temp.path(), "module", "..", "1");
        unsafe_package.store_path = source.clone();
        assert!(matches!(
            reconcile(manifest(vec![unsafe_package]), &data, &state),
            Err(ReconcileError::UnsafeId(id)) if id == ".."
        ));
        let mut package = p(temp.path(), "module", "demo", "1");
        package.store_path = source;
        assert!(matches!(
            reconcile(manifest(vec![package.clone(), package]), &data, &state),
            Err(ReconcileError::UnsafeId(message)) if message.contains("duplicate")
        ));
    }
}
