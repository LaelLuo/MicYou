use super::{RouteOutcome, RouteState};
use serde::{Deserialize, Serialize};
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Owner {
    pub pid: u32,
    pub created: u64,
    pub instance: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct RoleLease {
    pub original: String,
    pub installed: String,
    pub touched: bool,
    pub owned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Record {
    pub version: u32,
    pub owner: Owner,
    pub phase: String,
    pub roles: [RoleLease; 3],
}

pub(super) trait Backend {
    fn try_lock(&mut self) -> Result<(), String>;
    fn unlock(&mut self);
    fn owner(&self) -> Owner;
    fn owner_alive(&self, owner: &Owner) -> Result<bool, String>;
    fn defaults(&self) -> Result<[String; 3], String>;
    fn default_render(&self, role: usize) -> Result<String, String>;
    fn set_render(&mut self, role: usize, endpoint: &str) -> Result<(), String>;
    fn external_changes(&mut self) -> [bool; 3];
    fn invalidated_endpoints(&mut self) -> Vec<String>;
    fn reset_notifications(&mut self);
}

pub(super) trait Journal {
    fn load(&self) -> Result<Option<Record>, String>;
    fn save(&mut self, record: &Record) -> Result<(), String>;
    fn remove(&mut self) -> Result<(), String>;
}

pub(super) struct LeaseMachine<B: Backend, J: Journal> {
    backend: B,
    journal: J,
    locked: bool,
    record: Option<Record>,
    outcome: RouteOutcome,
    released_external: bool,
    stop_pending: bool,
}

impl<B: Backend, J: Journal> LeaseMachine<B, J> {
    pub fn new(backend: B, journal: J) -> Self {
        Self {
            backend,
            journal,
            locked: false,
            record: None,
            outcome: RouteOutcome::new(RouteState::Idle, None),
            released_external: false,
            stop_pending: false,
        }
    }

    pub fn status(&self) -> RouteOutcome {
        self.outcome.clone()
    }

    fn lock(&mut self) -> Result<(), String> {
        if !self.locked {
            self.backend.try_lock()?;
            self.locked = true;
        }
        Ok(())
    }

    fn unlock(&mut self) {
        if self.locked {
            self.backend.unlock();
            self.locked = false;
        }
    }

    fn failed(&mut self, error: String) -> RouteOutcome {
        self.outcome = RouteOutcome::new(
            RouteState::RestoreFailed,
            Some(format!("route_restore_failed: {error}")),
        );
        self.outcome.clone()
    }

    pub fn recover_stale(&mut self) -> RouteOutcome {
        if self.record.is_some() {
            return self.release();
        }
        if let Err(error) = self.lock() {
            return self.failed(error);
        }
        let record = match self.journal.load() {
            Ok(Some(record)) => record,
            Ok(None) => {
                self.unlock();
                self.outcome = RouteOutcome::new(RouteState::Idle, None);
                return self.outcome.clone();
            }
            Err(error) => return self.failed(error),
        };
        match self.backend.owner_alive(&record.owner) {
            Ok(false) => {
                self.record = Some(record);
                self.restore(false)
            }
            Ok(true) => {
                self.unlock();
                self.failed("route_owner_active: routing journal owner is still alive".into())
            }
            Err(error) => self.failed(error),
        }
    }

    pub fn acquire(&mut self, target: &str) -> Result<RouteOutcome, String> {
        if self.outcome.state == RouteState::RestoreFailed {
            return Err(self
                .outcome
                .error
                .clone()
                .unwrap_or_else(|| "routing recovery must be retried first".into()));
        }
        if self.stop_pending && self.record.is_some() {
            return Err("route_changed_external: release yielded routing lease first".into());
        }
        if target.is_empty() || target.contains('\0') || target.len() > 2048 {
            return Err("invalid render endpoint ID".into());
        }
        if let Some(record) = &self.record {
            if self.outcome.state == RouteState::Held
                && record.roles.iter().all(|role| role.installed == target)
            {
                return Ok(self.outcome.clone());
            }
            return Err(self
                .outcome
                .error
                .clone()
                .unwrap_or_else(|| "release existing routing lease first".into()));
        }
        self.lock()?;
        // 取锁后才允许读取或修改用户级恢复记录。
        match self.journal.load() {
            Ok(Some(_)) => {
                let result = self.recover_stale();
                if result.state == RouteState::RestoreFailed {
                    return Err(result.error.unwrap_or_default());
                }
                self.lock()?;
            }
            Ok(None) => {}
            Err(error) => {
                self.failed(error.clone());
                return Err(error);
            }
        }
        self.backend.reset_notifications();
        self.released_external = false;
        self.stop_pending = false;
        let original = match self.backend.defaults() {
            Ok(original) => original,
            Err(error) => {
                self.unlock();
                return Err(error);
            }
        };
        if original.iter().any(|id| id.eq_ignore_ascii_case(target)) {
            self.unlock();
            return Err("route_restore_target_required: select a normal playback device before automatic routing".into());
        }
        let record = Record {
            version: 1,
            owner: self.backend.owner(),
            phase: "acquiring".into(),
            roles: original.map(|id| RoleLease {
                original: id,
                installed: target.into(),
                touched: false,
                owned: true,
            }),
        };
        if let Err(error) = self.journal.save(&record) {
            self.unlock();
            return Err(error);
        }
        self.record = Some(record);
        for role in 0..3 {
            if let Err(error) = self.install_role(role, target) {
                let rollback = self.restore(false);
                return Err(match rollback.error {
                    Some(restore) => format!("{error}; recovery: {restore}"),
                    None => error,
                });
            }
        }
        self.record.as_mut().unwrap().phase = "held".into();
        if let Err(error) = self.journal.save(self.record.as_ref().unwrap()) {
            self.restore(false);
            return Err(error);
        }
        self.outcome = RouteOutcome::new(RouteState::Held, None);
        Ok(self.outcome.clone())
    }

    fn install_role(&mut self, role: usize, target: &str) -> Result<(), String> {
        if self.observe_external()? {
            return Err("computer playback device changed externally".into());
        }
        if !self
            .backend
            .default_render(role)?
            .eq_ignore_ascii_case(&self.record.as_ref().unwrap().roles[role].original)
        {
            self.record.as_mut().unwrap().roles[role].owned = false;
            self.released_external = true;
            self.journal.save(self.record.as_ref().unwrap())?;
            return Err("computer playback device changed externally".into());
        }
        let record = self.record.as_mut().unwrap();
        record.roles[role].touched = true;
        self.journal.save(record)?;
        self.backend.set_render(role, target)?;
        if !self
            .backend
            .default_render(role)?
            .eq_ignore_ascii_case(target)
        {
            return Err(format!("render role {role} switch verification failed"));
        }
        if self.observe_external()? {
            return Err("computer playback device changed externally".into());
        }
        Ok(())
    }

    fn observe_external(&mut self) -> Result<bool, String> {
        let changes = self.backend.external_changes();
        let invalidated = self.backend.invalidated_endpoints();
        let Some(record) = &mut self.record else {
            return Ok(false);
        };
        let unavailable = invalidated.iter().any(|id| {
            id.is_empty()
                || record.roles.iter().any(|role| {
                    role.original.eq_ignore_ascii_case(id)
                        || role.installed.eq_ignore_ascii_case(id)
                })
        });
        let mut changed = false;
        for (role, external) in record.roles.iter_mut().zip(changes) {
            if external && role.owned {
                role.owned = false;
                changed = true;
            }
        }
        if changed {
            self.released_external = true;
            self.journal.save(record)?;
        }
        // 原恢复设备消失只要求停止与尝试恢复，不能误当用户让出所有权。
        self.stop_pending |= changed || unavailable;
        if unavailable {
            self.outcome.error = Some("route_device_unavailable".into());
        }
        Ok(changed || unavailable)
    }

    pub fn external_change(&mut self) -> bool {
        if self.outcome.state != RouteState::Held {
            return false;
        }
        if self.stop_pending {
            return true;
        }
        match self.observe_external() {
            Ok(false) => false,
            // core 必须先失效 PCM 代际，再调用 release 恢复其余角色。
            Ok(true) => true,
            Err(error) => {
                self.failed(error);
                true
            }
        }
    }

    pub fn release(&mut self) -> RouteOutcome {
        if self.record.is_none() {
            return if self.outcome.state == RouteState::RestoreFailed {
                self.recover_stale()
            } else {
                self.outcome.clone()
            };
        }
        self.restore(self.outcome.state == RouteState::ReleasedExternal)
    }

    fn restore(&mut self, external: bool) -> RouteOutcome {
        if let Err(error) = self.observe_external() {
            return self.failed(error);
        }
        let external = external || self.released_external;
        let Some(mut record) = self.record.take() else {
            return self.outcome.clone();
        };
        record.phase = "restoring".into();
        if let Err(error) = self.journal.save(&record) {
            self.record = Some(record);
            return self.failed(error);
        }
        let mut failures = Vec::new();
        for role in 0..3 {
            let changes = self.backend.external_changes();
            for (entry, changed) in record.roles.iter_mut().zip(changes) {
                if changed {
                    entry.owned = false;
                }
            }
            if let Err(error) = self.journal.save(&record) {
                failures.push(error);
                break;
            }
            let entry = &mut record.roles[role];
            if !entry.touched || !entry.owned {
                continue;
            }
            match self.backend.default_render(role) {
                Ok(current) if current.eq_ignore_ascii_case(&entry.installed) => {
                    match self
                        .backend
                        .set_render(role, &entry.original)
                        .and_then(|_| self.backend.default_render(role))
                    {
                        Ok(current) if current.eq_ignore_ascii_case(&entry.original) => {
                            entry.touched = false
                        }
                        Ok(_) => {
                            failures.push(format!("render role {role} restore verification failed"))
                        }
                        Err(error) => {
                            failures.push(format!("render role {role} restore failed: {error}"))
                        }
                    }
                }
                Ok(_) => entry.owned = false,
                Err(error) => failures.push(error),
            }
            if let Err(error) = self.journal.save(&record) {
                failures.push(error);
                break;
            }
        }
        if failures.is_empty() {
            if let Err(error) = self.journal.remove() {
                self.record = Some(record);
                return self.failed(error);
            }
            self.unlock();
            self.outcome = RouteOutcome::new(
                if external {
                    RouteState::ReleasedExternal
                } else {
                    RouteState::Restored
                },
                None,
            );
            self.outcome.clone()
        } else {
            record.phase = "restore_failed".into();
            let _ = self.journal.save(&record);
            self.record = Some(record);
            self.failed(failures.join("; "))
        }
    }
}
