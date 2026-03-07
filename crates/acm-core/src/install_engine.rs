use anyhow::{Result, anyhow, bail};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use tar::Archive;
use tempfile::NamedTempFile;
use zip::ZipArchive;

#[derive(Debug, Clone)]
pub struct InstallRequest<'a> {
    pub provider: &'a str,
    pub version: &'a str,
    pub archive_path: &'a Path,
    pub install_dir: &'a Path,
    pub bin_dir: &'a Path,
}

#[derive(Debug, Clone)]
pub struct InstallResult {
    pub install_root: PathBuf,
    pub executable: PathBuf,
    pub bin_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct UninstallRequest<'a> {
    pub provider: &'a str,
    pub install_dir: &'a Path,
    pub install_path: &'a Path,
    pub bin_dir: &'a Path,
}

#[derive(Debug, Clone)]
pub struct UninstallResult {
    pub install_removed: bool,
    pub bin_removed: bool,
    pub bin_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ImportRequest<'a> {
    pub provider: &'a str,
    pub version: &'a str,
    pub from: &'a Path,
    pub install_dir: &'a Path,
    pub bin_dir: &'a Path,
}

#[derive(Debug, Clone)]
pub struct ImportResult {
    pub install_root: PathBuf,
    pub executable: PathBuf,
    pub bin_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct UpdateRequest<'a> {
    pub provider: &'a str,
    pub version: &'a str,
    pub archive_path: &'a Path,
    pub install_dir: &'a Path,
    pub bin_dir: &'a Path,
}

#[derive(Debug, Clone)]
pub struct UpdateResult {
    pub install: InstallResult,
}

pub fn install_from_archive(req: &InstallRequest<'_>) -> Result<InstallResult> {
    let install_root = install_root_for(req.install_dir, req.provider, req.version);
    if install_root.exists() {
        fs::remove_dir_all(&install_root)?;
    }
    fs::create_dir_all(&install_root)?;

    let executable = install_artifact(req.archive_path, &install_root, req.provider)?;
    let bin_path = activate_executable(req.bin_dir, req.provider, &executable)?;

    Ok(InstallResult {
        install_root,
        executable,
        bin_path,
    })
}

pub fn update_from_archive(req: &UpdateRequest<'_>) -> Result<UpdateResult> {
    let install = install_from_archive(&InstallRequest {
        provider: req.provider,
        version: req.version,
        archive_path: req.archive_path,
        install_dir: req.install_dir,
        bin_dir: req.bin_dir,
    })?;
    Ok(UpdateResult { install })
}

pub fn uninstall_provider(req: &UninstallRequest<'_>) -> Result<UninstallResult> {
    let mut install_removed = false;
    if req.install_path.exists() {
        fs::remove_dir_all(req.install_path)?;
        install_removed = true;
    }

    let mut bin_path = bin_path_for_provider(req.bin_dir, req.provider);
    let mut bin_removed = false;
    if fs::symlink_metadata(&bin_path).is_ok() {
        bin_removed = remove_bin_with_retry(&bin_path)?;
    } else if let Some(bin_install_dir) =
        derive_install_dir_from_install_path(req.install_path, req.provider)
    {
        let fallback_bin_path = bin_path_for_provider(&bin_dir(&bin_install_dir), req.provider);
        if fallback_bin_path != bin_path && fs::symlink_metadata(&fallback_bin_path).is_ok() {
            bin_removed = remove_bin_with_retry(&fallback_bin_path)?;
            bin_path = fallback_bin_path;
        }
    }

    Ok(UninstallResult {
        install_removed,
        bin_removed,
        bin_path,
    })
}

fn derive_install_dir_from_install_path(install_path: &Path, provider: &str) -> Option<PathBuf> {
    let provider_dir = install_path.parent()?;
    let provider_name = provider_dir.file_name()?.to_str()?;
    if provider_name != provider {
        return None;
    }

    let providers_dir = provider_dir.parent()?;
    let providers_name = providers_dir.file_name()?.to_str()?;
    if providers_name != "providers" {
        return None;
    }

    Some(providers_dir.parent()?.to_path_buf())
}

fn remove_bin_with_retry(path: &Path) -> Result<bool> {
    if fs::symlink_metadata(path).is_err() {
        return Ok(false);
    }

    let attempts = if cfg!(windows) { 10 } else { 1 };
    for attempt in 0..attempts {
        match fs::remove_file(path) {
            Ok(()) => return Ok(true),
            Err(err) if attempt + 1 < attempts && cfg!(windows) => {
                std::thread::sleep(std::time::Duration::from_millis(300));
                if fs::symlink_metadata(path).is_err() {
                    return Ok(true);
                }
                tracing::debug!(
                    "retrying remove_file for {} after error: {}",
                    path.display(),
                    err
                );
            }
            Err(err) => return Err(err.into()),
        }
    }

    Ok(fs::symlink_metadata(path).is_err())
}

pub fn import_from_dir(req: &ImportRequest<'_>) -> Result<ImportResult> {
    if !req.from.exists() || !req.from.is_dir() {
        bail!("import source directory not found: {}", req.from.display());
    }

    let install_root = install_root_for(req.install_dir, req.provider, req.version);
    if install_root.exists() {
        fs::remove_dir_all(&install_root)?;
    }
    fs::create_dir_all(&install_root)?;

    copy_dir_recursive(req.from, &install_root)?;
    let executable = locate_executable(&install_root, req.provider)?;
    let bin_path = activate_executable(req.bin_dir, req.provider, &executable)?;

    Ok(ImportResult {
        install_root,
        executable,
        bin_path,
    })
}

pub fn cache_path_for(cache_dir: &Path, provider: &str, version: &str, filename: &str) -> PathBuf {
    let mut path = cache_dir.join("downloads").join(provider).join(version);
    for segment in filename.split('/') {
        path = path.join(segment);
    }
    path
}

pub fn install_root_for(install_dir: &Path, provider: &str, version: &str) -> PathBuf {
    install_dir.join("providers").join(provider).join(version)
}

pub fn bin_dir(install_dir: &Path) -> PathBuf {
    install_dir.join("bin")
}

pub fn bin_path_for_provider(bin_dir: &Path, provider: &str) -> PathBuf {
    if cfg!(windows) {
        bin_dir.join(format!("{}.exe", provider))
    } else {
        bin_dir.join(provider)
    }
}

fn install_artifact(artifact_path: &Path, install_root: &Path, provider: &str) -> Result<PathBuf> {
    let filename = artifact_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if filename.ends_with(".zip")
        || filename.ends_with(".tar.gz")
        || filename.ends_with(".tgz")
        || filename.ends_with(".tar.xz")
    {
        let extract_root = install_root.join("extract");
        fs::create_dir_all(&extract_root)?;
        extract_archive(artifact_path, &extract_root)?;
        return locate_executable(&extract_root, provider);
    }

    let target_name = if cfg!(windows) {
        format!("{}.exe", provider)
    } else {
        provider.to_string()
    };
    let target_path = install_root.join(target_name);
    copy_file_atomic(artifact_path, &target_path)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&target_path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&target_path, permissions)?;
    }

    Ok(target_path)
}

fn locate_executable(root: &Path, provider: &str) -> Result<PathBuf> {
    let mut files = Vec::new();
    collect_files(root, &mut files)?;

    let mut candidates = Vec::new();
    let provider_lower = provider.to_ascii_lowercase();
    for file in &files {
        let Some(name) = file.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let name_lower = name.to_ascii_lowercase();
        if name_lower == provider_lower || name_lower == format!("{}.exe", provider_lower) {
            candidates.push(file.clone());
        }
    }
    if let Some(path) = candidates.into_iter().next() {
        return Ok(path);
    }

    for file in &files {
        if is_executable_file(file)? {
            return Ok(file.clone());
        }
    }

    files
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no file found after extraction"))
}

fn collect_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn is_executable_file(path: &Path) -> Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path)?.permissions().mode();
        Ok((mode & 0o111) != 0)
    }

    #[cfg(windows)]
    {
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        Ok(ext == "exe" || ext == "bat" || ext == "cmd")
    }
}

fn activate_executable(bin_dir: &Path, provider: &str, executable: &Path) -> Result<PathBuf> {
    fs::create_dir_all(bin_dir)?;

    let link_path = bin_path_for_provider(bin_dir, provider);
    if link_path.exists() {
        let _ = fs::remove_file(&link_path);
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(executable, &link_path)
            .map_err(|e| anyhow!("create symlink failed: {}: {}", link_path.display(), e))?;
    }

    #[cfg(windows)]
    {
        copy_file_atomic(executable, &link_path)?;
    }

    Ok(link_path)
}

fn extract_archive(archive_path: &Path, destination: &Path) -> Result<()> {
    let filename = archive_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if filename.ends_with(".zip") {
        let file = File::open(archive_path)?;
        let mut zip = ZipArchive::new(file)?;
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index)?;
            let output = destination.join(entry.mangled_name());
            if entry.name().ends_with('/') {
                fs::create_dir_all(&output)?;
                continue;
            }
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = File::create(&output)?;
            std::io::copy(&mut entry, &mut file)?;
        }
        return Ok(());
    }

    if filename.ends_with(".tar.gz") || filename.ends_with(".tgz") {
        let file = File::open(archive_path)?;
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = Archive::new(decoder);
        archive.unpack(destination)?;
        return Ok(());
    }

    if filename.ends_with(".tar.xz") {
        let file = File::open(archive_path)?;
        let decoder = xz2::read::XzDecoder::new(file);
        let mut archive = Archive::new(decoder);
        archive.unpack(destination)?;
        return Ok(());
    }

    bail!("unsupported archive format: {}", archive_path.display())
}

fn copy_file_atomic(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut temp = NamedTempFile::new_in(
        to.parent()
            .ok_or_else(|| anyhow!("target has no parent: {}", to.display()))?,
    )?;
    {
        let mut source = File::open(from)?;
        std::io::copy(&mut source, &mut temp)?;
    }

    temp.persist(to)
        .map_err(|err| anyhow!("persist failed for {}: {}", to.display(), err))?;

    Ok(())
}

fn copy_dir_recursive(from: &Path, to: &Path) -> Result<()> {
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = to.join(entry.file_name());
        if source_path.is_dir() {
            fs::create_dir_all(&target_path)?;
            copy_dir_recursive(&source_path, &target_path)?;
        } else if source_path.is_file() {
            copy_file_atomic(&source_path, &target_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_path_for_nested_filename() {
        let base = PathBuf::from("/tmp/acm-cache");
        let path = cache_path_for(&base, "tool-a", "1.2.3", "nested/asset.tar.xz");
        assert!(path.ends_with("downloads/tool-a/1.2.3/nested/asset.tar.xz"));
    }

    #[test]
    fn test_install_and_uninstall_single_binary() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let install_dir = temp.path().join("install");
        fs::create_dir_all(&install_dir).expect("create install dir");

        let source = temp.path().join("tool-a.bin");
        fs::write(&source, b"echo tool-a").expect("write source artifact");

        let bin_dir = install_dir.join("bin");

        let install = install_from_archive(&InstallRequest {
            provider: "tool-a",
            version: "1.0.0",
            archive_path: &source,
            install_dir: &install_dir,
            bin_dir: &bin_dir,
        })
        .expect("install should succeed");

        assert!(install.install_root.exists());
        assert!(install.executable.exists());
        assert!(install.bin_path.exists());

        let uninstall = uninstall_provider(&UninstallRequest {
            provider: "tool-a",
            install_dir: &install_dir,
            install_path: &install.install_root,
            bin_dir: &bin_dir,
        })
        .expect("uninstall should succeed");

        assert!(uninstall.install_removed);
        assert!(uninstall.bin_removed);
        assert!(!install.install_root.exists());
        assert!(!install.bin_path.exists());
    }

    #[test]
    fn test_uninstall_uses_install_path_to_remove_custom_install_dir_bin() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let install_dir = temp.path().join("custom-install");
        let fallback_install_dir = temp.path().join("default-install");
        fs::create_dir_all(&install_dir).expect("create install dir");
        fs::create_dir_all(&fallback_install_dir).expect("create fallback install dir");

        let source = temp.path().join("tool-c.bin");
        fs::write(&source, b"echo tool-c").expect("write source artifact");

        let bin_dir = install_dir.join("bin");

        let install = install_from_archive(&InstallRequest {
            provider: "tool-c",
            version: "1.0.0",
            archive_path: &source,
            install_dir: &install_dir,
            bin_dir: &bin_dir,
        })
        .expect("install should succeed");

        let uninstall = uninstall_provider(&UninstallRequest {
            provider: "tool-c",
            install_dir: &fallback_install_dir,
            install_path: &install.install_root,
            bin_dir: &fallback_install_dir.join("bin"),
        })
        .expect("uninstall should succeed");

        assert!(uninstall.install_removed);
        assert!(uninstall.bin_removed);
        assert!(!install.install_root.exists());
        assert!(!install.bin_path.exists());
    }

    #[test]
    fn test_install_and_uninstall_with_separate_bin_dir() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let install_dir = temp.path().join("install-root");
        let bin_dir = temp.path().join("agents-home").join("bin");
        fs::create_dir_all(&install_dir).expect("create install dir");
        fs::create_dir_all(&bin_dir).expect("create bin dir");

        let source = temp.path().join("tool-d.bin");
        fs::write(&source, b"echo tool-d").expect("write source artifact");

        let install = install_from_archive(&InstallRequest {
            provider: "tool-d",
            version: "1.0.0",
            archive_path: &source,
            install_dir: &install_dir,
            bin_dir: &bin_dir,
        })
        .expect("install should succeed");

        assert!(install.install_root.starts_with(&install_dir));
        assert!(install.bin_path.starts_with(&bin_dir));
        assert!(install.bin_path.exists());

        let uninstall = uninstall_provider(&UninstallRequest {
            provider: "tool-d",
            install_dir: &install_dir,
            install_path: &install.install_root,
            bin_dir: &bin_dir,
        })
        .expect("uninstall should succeed");

        assert!(uninstall.install_removed);
        assert!(uninstall.bin_removed);
        assert!(!install.install_root.exists());
        assert!(!install.bin_path.exists());
    }

    #[test]
    fn test_import_from_dir() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let install_dir = temp.path().join("install");
        let from_dir = temp.path().join("source");
        fs::create_dir_all(&install_dir).expect("create install dir");
        fs::create_dir_all(&from_dir).expect("create source dir");

        let source_bin = if cfg!(windows) {
            from_dir.join("tool-b.exe")
        } else {
            from_dir.join("tool-b")
        };
        fs::write(&source_bin, b"tool-b").expect("write source binary");

        let bin_dir = install_dir.join("bin");

        let imported = import_from_dir(&ImportRequest {
            provider: "tool-b",
            version: "2.0.0",
            from: &from_dir,
            install_dir: &install_dir,
            bin_dir: &bin_dir,
        })
        .expect("import should succeed");

        assert!(imported.install_root.exists());
        assert!(imported.executable.exists());
        assert!(imported.bin_path.exists());
    }
}
