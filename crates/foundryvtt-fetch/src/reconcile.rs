//! Fail-closed reconciliation of immutable Foundry module/system outputs.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io,
    os::unix::fs as unix_fs,
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
    #[error("managed package target is occupied by an unmanaged path: {0}")]
    ForeignPath(PathBuf),
    #[error("managed package target is not the recorded symlink: {0}")]
    StateMismatch(PathBuf),
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

/// Reconcile only links below `Data/modules` and `Data/systems`. Foreign paths
/// are never removed, and package data in the immutable store is untouched.
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
    let mut map = BTreeMap::new();
    let mut tombstones = BTreeMap::new();
    for package in &desired.packages {
        validate_package(package)?;
        let key = key(&package.kind, &package.id);
        if declared.insert(key.clone(), package).is_some() {
            return Err(ReconcileError::UnsafeId(format!("duplicate package {key}")));
        }
        if package.state == "present" {
            map.insert(key, package);
        } else {
            tombstones.insert(key, package);
        }
    }
    // Preflight every target so a foreign collision cannot partially update a version.
    for package in map.values() {
        check_desired(
            &target(data_dir, package),
            package,
            previous.packages.get(&key(&package.kind, &package.id)),
        )?;
    }
    for name in tombstones.keys() {
        if let Some(previous_package) = previous.packages.get(name) {
            check_remove(&target(data_dir, previous_package), previous_package)?;
        }
    }
    // Undeclared packages are intentionally preserved. Deletion is explicit
    // through a state = "absent" tombstone.
    for package in map.values() {
        install_link(&target(data_dir, package), &package.store_path)?;
    }
    for name in tombstones.keys() {
        if let Some(package) = previous.packages.get(name) {
            let path = target(data_dir, package);
            if path.symlink_metadata().is_ok() {
                fs::remove_file(path)?;
            }
        }
    }
    let mut packages = previous.packages.clone();
    for name in tombstones.keys() {
        packages.remove(name);
    }
    for (name, p) in map {
        packages.insert(
            name,
            ManagedPackage {
                kind: p.kind.clone(),
                id: p.id.clone(),
                version: p.version.clone(),
                store_path: p.store_path.clone(),
            },
        );
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
    let metadata = fs::symlink_metadata(&package.store_path)
        .map_err(|_| ReconcileError::InvalidStorePath(package.id.clone()))?;
    if !metadata.file_type().is_dir() || package.store_path.is_relative() {
        return Err(ReconcileError::InvalidStorePath(package.id.clone()));
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
impl PackageLike for ManagedPackage {
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

fn check_desired(
    path: &Path,
    package: &DesiredPackage,
    previous: Option<&ManagedPackage>,
) -> Result<(), ReconcileError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if !metadata.file_type().is_symlink() {
        return Err(ReconcileError::ForeignPath(path.to_path_buf()));
    }
    let link = fs::read_link(path)?;
    if link == package.store_path || previous.is_some_and(|p| link == p.store_path) {
        Ok(())
    } else {
        Err(ReconcileError::ForeignPath(path.to_path_buf()))
    }
}

fn check_remove(path: &Path, package: &ManagedPackage) -> Result<(), ReconcileError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if !metadata.file_type().is_symlink() || fs::read_link(path)? != package.store_path {
        return Err(ReconcileError::StateMismatch(path.to_path_buf()));
    }
    Ok(())
}

fn install_link(target: &Path, source: &Path) -> Result<(), ReconcileError> {
    let parent = target
        .parent()
        .ok_or_else(|| ReconcileError::ForeignPath(target.to_path_buf()))?;
    fs::create_dir_all(parent)?;
    let temp = NamedTempFile::new_in(parent)?;
    let temp_path = temp.into_temp_path();
    fs::remove_file(&temp_path)?;
    unix_fs::symlink(source, &temp_path).map_err(|e| {
        if e.kind() == io::ErrorKind::Unsupported {
            ReconcileError::UnsupportedPlatform
        } else {
            ReconcileError::Io(e)
        }
    })?;
    fs::rename(&temp_path, target)?;
    Ok(())
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
    #[test]
    fn add_version_delete_and_refuse_foreign_path() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("1")).unwrap();
        fs::create_dir_all(temp.path().join("2")).unwrap();
        let data = temp.path().join("data");
        let state = temp.path().join("state.json");
        let manifest = |packages| DesiredManifest {
            schema_version: SCHEMA_VERSION,
            packages,
        };
        let world = data.join("Data/worlds/kept/world.json");
        fs::create_dir_all(world.parent().unwrap()).unwrap();
        fs::write(&world, b"foundry-owned").unwrap();
        reconcile(
            manifest(vec![p(temp.path(), "module", "demo", "1")]),
            &data,
            &state,
        )
        .unwrap();
        assert_eq!(
            fs::read_link(data.join("Data/modules/demo")).unwrap(),
            temp.path().join("1")
        );
        reconcile(
            manifest(vec![p(temp.path(), "module", "demo", "2")]),
            &data,
            &state,
        )
        .unwrap();
        assert_eq!(
            fs::read_link(data.join("Data/modules/demo")).unwrap(),
            temp.path().join("2")
        );
        reconcile(
            manifest(vec![p(temp.path(), "module", "demo", "1")]),
            &data,
            &state,
        )
        .unwrap();
        assert_eq!(
            fs::read_link(data.join("Data/modules/demo")).unwrap(),
            temp.path().join("1")
        );
        // Omitting a package preserves it; deletion requires a tombstone.
        reconcile(manifest(vec![]), &data, &state).unwrap();
        assert!(data.join("Data/modules/demo").exists());
        reconcile(
            manifest(vec![DesiredPackage {
                kind: "module".into(),
                id: "demo".into(),
                state: "absent".into(),
                version: String::new(),
                store_path: PathBuf::new(),
            }]),
            &data,
            &state,
        )
        .unwrap();
        assert!(!data.join("Data/modules/demo").exists());
        assert_eq!(fs::read(&world).unwrap(), b"foundry-owned");
        fs::create_dir_all(data.join("Data/modules/demo")).unwrap();
        assert!(matches!(
            reconcile(
                manifest(vec![p(temp.path(), "module", "demo", "1")]),
                &data,
                &state
            ),
            Err(ReconcileError::ForeignPath(_))
        ));
    }

    #[test]
    fn manages_systems_and_rejects_state_mismatch() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("1")).unwrap();
        fs::create_dir_all(temp.path().join("foreign")).unwrap();
        let data = temp.path().join("data");
        let state = temp.path().join("state.json");
        let manifest = |packages| DesiredManifest {
            schema_version: SCHEMA_VERSION,
            packages,
        };
        reconcile(
            manifest(vec![p(temp.path(), "system", "rules", "1")]),
            &data,
            &state,
        )
        .unwrap();
        let target = data.join("Data/systems/rules");
        fs::remove_file(&target).unwrap();
        unix_fs::symlink(temp.path().join("foreign"), &target).unwrap();
        assert!(matches!(
            reconcile(
                manifest(vec![DesiredPackage {
                    kind: "system".into(),
                    id: "rules".into(),
                    state: "absent".into(),
                    version: String::new(),
                    store_path: PathBuf::new(),
                }]),
                &data,
                &state,
            ),
            Err(ReconcileError::StateMismatch(path)) if path == target
        ));
    }

    #[test]
    fn rejects_dot_path_components_and_duplicate_packages() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("1")).unwrap();
        let data = temp.path().join("data");
        let state = temp.path().join("state.json");
        let manifest = |packages| DesiredManifest {
            schema_version: SCHEMA_VERSION,
            packages,
        };
        assert!(matches!(
            reconcile(manifest(vec![p(temp.path(), "module", "..", "1")]), &data, &state),
            Err(ReconcileError::UnsafeId(id)) if id == ".."
        ));
        let package = p(temp.path(), "module", "demo", "1");
        assert!(matches!(
            reconcile(manifest(vec![package.clone(), package]), &data, &state),
            Err(ReconcileError::UnsafeId(message)) if message.contains("duplicate")
        ));
    }
}
