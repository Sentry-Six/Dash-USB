//! Offline archive-script refresh for the release being installed.
use std::{fs, io::Write, path::Path};
use anyhow::{Context, Result, ensure};
use crate::archive::ArchiveSystem;

fn scripts(system: ArchiveSystem) -> Vec<(&'static str, &'static str)> {
    let mut scripts = crate::scripts::archive_runtime_scripts().to_vec();
    scripts.extend_from_slice(crate::archive::archive_scripts(system));
    // Publish dependencies before the loop that sources them.
    scripts.sort_by_key(|(name, _)| *name == "archiveloop");
    scripts
}

pub fn script_names(system: ArchiveSystem) -> Vec<&'static str> {
    scripts(system).into_iter().map(|(name, _)| name).collect()
}

fn configured_system() -> Result<ArchiveSystem> {
    let config_path = sentryusb_config::find_config_path();
    ensure!(Path::new(config_path).is_file(), "Archive configuration is unavailable: {config_path}");
    let (config, _) = sentryusb_config::parse_file(config_path)?;
    ArchiveSystem::from_config(config.get("ARCHIVE_SYSTEM").map(String::as_str).unwrap_or("none"))
}

pub fn configured_is_current(directory: &Path) -> Result<bool> {
    Ok(contents_current(directory, &scripts(configured_system()?)))
}

pub fn refresh_configured(directory: &Path) -> Result<()> {
    refresh(directory, configured_system()?)
}

fn contents_current(directory: &Path, scripts: &[(&str, &str)]) -> bool {
    scripts.iter().all(|(name, content)| {
        let path = directory.join(name);
        let matches = fs::read_to_string(&path).is_ok_and(|text| text == *content);
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            matches && fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 == 0o111)
        }
        #[cfg(not(unix))] matches
    })
}

pub fn refresh(directory: &Path, system: ArchiveSystem) -> Result<()> {
    let scripts = scripts(system);
    if contents_current(directory, &scripts) { return Ok(()); }
    fs::create_dir_all(directory)?;
    // The updater and startup migration may run in separate processes. Keep
    // their staged generations from interleaving; never remove the lock inode.
    let lock = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .open(directory.join(".archive-runtime.lock"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            },
            Err(error) => anyhow::bail!("Archive runtime refresh lock unavailable: {error}"),
        }
    }
    if contents_current(directory, &scripts) { return Ok(()); }
    install(directory, &scripts)
}

fn install(directory: &Path, scripts: &[(&str, &str)]) -> Result<()> {
    fs::create_dir_all(directory)?;
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
    let stage = directory.join(format!(".archive-runtime-{}-{nonce}", std::process::id()));
    fs::create_dir(&stage)?;
    let mut replaced = Vec::new();
    let result = (|| -> Result<()> {
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&stage, fs::Permissions::from_mode(0o700))?;
        }
        for (name, content) in scripts {
            ensure!(!name.contains('/') && !name.starts_with('.'), "Invalid runtime script name");
            let path = stage.join(name);
            let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
            file.write_all(content.as_bytes())?;
            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(0o755))?;
            }
            file.sync_all()?;
            let destination = directory.join(name);
            match fs::symlink_metadata(&destination) {
                Ok(metadata) => {
                    ensure!(metadata.is_file() || metadata.is_symlink(), "Unexpected runtime destination: {}", destination.display());
                    let backup = stage.join(format!("{name}.previous"));
                    #[cfg(unix)]
                    if metadata.is_symlink() {
                        std::os::unix::fs::symlink(fs::read_link(&destination)?, backup)?;
                        continue;
                    }
                    fs::copy(&destination, &backup)?;
                    fs::File::open(backup)?.sync_all()?;
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => return Err(error.into()),
            }
        }
        sync_directory(&stage)?;
        for (name, _) in scripts {
            fs::rename(stage.join(name), directory.join(name))?;
            replaced.push(*name);
            sync_directory(directory)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let mut rollback_errors = Vec::new();
        for name in replaced.into_iter().rev() {
            let backup = stage.join(format!("{name}.previous"));
            let rollback = if fs::symlink_metadata(&backup).is_ok() {
                fs::rename(backup, directory.join(name))
            } else { fs::remove_file(directory.join(name)) };
            if let Err(error) = rollback { rollback_errors.push(error.to_string()); }
        }
        if let Err(error) = sync_directory(directory) { rollback_errors.push(error.to_string()); }
        if !rollback_errors.is_empty() {
            anyhow::bail!("{error:#}; rollback failed: {}; recovery files kept at {}", rollback_errors.join("; "), stage.display());
        }
        let _ = fs::remove_dir_all(&stage);
        return Err(error.context("Archive scripts were not refreshed"));
    }
    fs::remove_dir_all(stage).context("Remove archive-script staging directory")?;
    sync_directory(directory)
}

fn sync_directory(directory: &Path) -> Result<()> {
    #[cfg(unix)] fs::File::open(directory)?.sync_all()?;
    #[cfg(not(unix))] let _ = directory;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temporary_directory() -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let directory = std::env::temp_dir().join(format!("archive-runtime-{}-{nonce}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        directory
    }

    #[test]
    fn installs_matching_backend_and_preserves_user_files_and_open_scripts() {
        for system in [ArchiveSystem::Cifs, ArchiveSystem::Nfs, ArchiveSystem::Rsync, ArchiveSystem::Rclone, ArchiveSystem::None] {
            let directory = temporary_directory();
            fs::write(directory.join("archiveloop"), "old loop").unwrap();
            let mut running_script = fs::File::open(directory.join("archiveloop")).unwrap();
            fs::write(directory.join("dashusb.conf"), "unchanged settings").unwrap();
            refresh(&directory, system).unwrap();
            let mut old_contents = String::new();
            std::io::Read::read_to_string(&mut running_script, &mut old_contents).unwrap();
            assert_eq!(old_contents, "old loop", "running shell keeps its original inode");
            for (name, content) in crate::archive::archive_scripts(system) {
                assert_eq!(fs::read_to_string(directory.join(name)).unwrap(), *content);
            }
            assert!(fs::read_to_string(directory.join("archiveloop")).unwrap().contains("archive_cycle_begin"));
            assert!(fs::read_to_string(directory.join("archive-control.sh")).unwrap().contains("archive_cancel_requested"));
            assert_eq!(fs::read_to_string(directory.join("dashusb.conf")).unwrap(), "unchanged settings");
            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(fs::metadata(directory.join("archiveloop")).unwrap().permissions().mode() & 0o777, 0o755);
            }
            let metadata = fs::metadata(directory.join("archiveloop")).unwrap();
            refresh(&directory, system).unwrap();
            #[cfg(unix)] {
                use std::os::unix::fs::MetadataExt;
                assert_eq!(metadata.ino(), fs::metadata(directory.join("archiveloop")).unwrap().ino(), "unchanged refresh does not replace running scripts");
            }
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn staging_failure_leaves_every_installed_script_unchanged() {
        let directory = temporary_directory();
        fs::write(directory.join("first"), "original").unwrap();
        fs::create_dir(directory.join("second")).unwrap();
        assert!(install(&directory, &[("first", "new"), ("second", "new")]).is_err());
        assert_eq!(fs::read_to_string(directory.join("first")).unwrap(), "original");
        assert!(directory.join("second").is_dir());
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
        fs::remove_dir_all(directory).unwrap();
    }
}
