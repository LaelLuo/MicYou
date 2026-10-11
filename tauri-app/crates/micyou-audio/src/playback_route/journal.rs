use super::lease::{Journal, Record};
use std::path::PathBuf;

pub(super) struct FileJournal {
    path: PathBuf,
}

impl FileJournal {
    pub fn new() -> Result<Self, String> {
        let root = std::env::var_os("LOCALAPPDATA").ok_or("LocalAppData unavailable")?;
        Ok(Self {
            path: PathBuf::from(root)
                .join("com.laelluo.micyou.duplex")
                .join("routing-lease.json"),
        })
    }
}

impl Journal for FileJournal {
    fn load(&self) -> Result<Option<Record>, String> {
        use std::io::Read;
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("routing journal read failed: {error}")),
        };
        let mut bytes = Vec::new();
        file.take(65537)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > 65536 {
            return Err("routing journal exceeds size limit".into());
        }
        let record: Record = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid routing journal: {error}"))?;
        if record.version != 1
            || record.owner.pid == 0
            || record.owner.created == 0
            || record.owner.instance.is_empty()
            || record.owner.instance.len() > 256
            || !["acquiring", "held", "restoring", "restore_failed"]
                .contains(&record.phase.as_str())
            || record.roles.iter().any(|role| {
                [&role.original, &role.installed]
                    .iter()
                    .any(|id| id.is_empty() || id.len() > 2048 || id.contains('\0'))
            })
        {
            return Err("invalid routing journal fields".into());
        }
        Ok(Some(record))
    }

    fn save(&mut self, record: &Record) -> Result<(), String> {
        use std::io::Write;
        std::fs::create_dir_all(self.path.parent().unwrap()).map_err(|error| error.to_string())?;
        let next = self.path.with_extension("json.next");
        let bytes = serde_json::to_vec(record).map_err(|error| error.to_string())?;
        let mut file = std::fs::File::create(&next).map_err(|error| error.to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|error| error.to_string())?;
        drop(file);
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
        }
        use std::os::windows::ffi::OsStrExt;
        let from: Vec<u16> = next.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = self.path.as_os_str().encode_wide().chain(Some(0)).collect();
        // 同目录原子替换并等待落盘，不能先删除旧事务再 rename。
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 1 | 8) } == 0 {
            return Err(format!(
                "routing journal replace failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    fn remove(&mut self) -> Result<(), String> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("routing journal removal failed: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback_route::lease::{Owner, RoleLease};

    fn isolated() -> FileJournal {
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join("playback-route-journal-tests");
        let path = root.join(suffix).join("routing-lease.json");
        assert!(path.starts_with(&root));
        FileJournal { path }
    }
    fn record() -> Record {
        Record {
            version: 1,
            owner: Owner {
                pid: 42,
                created: 123,
                instance: "test".into(),
            },
            phase: "acquiring".into(),
            roles: ["speakers", "headphones", "communications"].map(|id| RoleLease {
                original: id.into(),
                installed: "hifi".into(),
                touched: true,
                owned: true,
            }),
        }
    }
    fn cleanup(journal: &FileJournal) {
        if journal.path.exists() {
            std::fs::remove_file(&journal.path).unwrap();
        }
        let next = journal.path.with_extension("json.next");
        if next.is_file() {
            std::fs::remove_file(&next).unwrap();
        }
        if next.is_dir() {
            std::fs::remove_dir(&next).unwrap();
        }
        std::fs::remove_dir(journal.path.parent().unwrap()).unwrap();
    }

    #[test]
    fn atomic_replacement_reloads_each_complete_record_and_failed_save_keeps_previous() {
        let mut journal = isolated();
        assert!(journal.load().unwrap().is_none());
        let mut snapshot = record();
        journal.save(&snapshot).unwrap();
        assert_eq!(journal.load().unwrap().unwrap().phase, "acquiring");
        snapshot.phase = "held".into();
        journal.save(&snapshot).unwrap();
        assert_eq!(journal.load().unwrap().unwrap().phase, "held");
        std::fs::create_dir(journal.path.with_extension("json.next")).unwrap();
        snapshot.phase = "restore_failed".into();
        assert!(journal.save(&snapshot).is_err());
        assert_eq!(journal.load().unwrap().unwrap().phase, "held");
        journal.remove().unwrap();
        assert!(journal.load().unwrap().is_none());
        cleanup(&journal);
    }

    #[test]
    fn corrupt_oversized_or_unrecognized_journal_never_becomes_a_restore_record() {
        let mut journal = isolated();
        journal.save(&record()).unwrap();
        for bytes in [
            b"{broken".to_vec(),
            vec![b' '; 65537],
            {
                let mut invalid = record();
                invalid.version = 2;
                serde_json::to_vec(&invalid).unwrap()
            },
            {
                let mut invalid = record();
                invalid.roles[0].original = "\0bad".into();
                serde_json::to_vec(&invalid).unwrap()
            },
        ] {
            std::fs::write(&journal.path, bytes).unwrap();
            assert!(journal.load().is_err());
            assert!(
                journal.path.exists(),
                "invalid recovery evidence must not be discarded"
            );
        }
        cleanup(&journal);
    }
}
