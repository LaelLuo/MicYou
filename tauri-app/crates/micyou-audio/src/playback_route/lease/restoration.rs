use super::*;

impl<B: Backend, J: Journal> LeaseMachine<B, J> {
    pub(super) fn restore_transaction(&mut self, external: bool) -> RouteOutcome {
        if let Err(error) = self.observe_external() {
            return self.failed(error);
        }
        let Some(mut record) = self.record.take() else {
            return self.outcome.clone();
        };
        record.phase = "restoring".into();
        if let Err(error) = self.journal.save(&record) {
            self.record = Some(record);
            return self.failed(error);
        }
        let mut visited = [false; 3];
        let mut failures = Vec::new();
        for role in 0..3 {
            if visited[role] {
                continue;
            }
            for (slot, affected) in visited.iter_mut().zip(self.backend.write_roles(role)) {
                *slot |= affected;
            }
            if let Err(error) = self.restore_group(&mut record, role) {
                failures.push(error);
            }
        }
        if !failures.is_empty() {
            record.phase = "restore_failed".into();
            if let Err(error) = self.journal.save(&record) {
                failures.push(error);
            }
            self.record = Some(record);
            return self.failed(failures.join("; "));
        }
        if let Err(error) = self.journal.remove() {
            self.record = Some(record);
            return self.failed(error);
        }
        self.unlock();
        self.outcome = RouteOutcome::new(
            if external || self.released_external {
                RouteState::ReleasedExternal
            } else {
                RouteState::Restored
            },
            None,
        );
        self.outcome.clone()
    }

    fn restore_group(&mut self, record: &mut Record, role: usize) -> Result<(), String> {
        let changes = self.backend.external_changes();
        for (index, changed) in changes.into_iter().enumerate() {
            if changed {
                for (entry, affected) in
                    record.roles.iter_mut().zip(self.backend.write_roles(index))
                {
                    if affected {
                        entry.owned = false;
                    }
                }
                self.released_external = true;
            }
        }
        self.journal.save(record)?;
        let affected = self.backend.write_roles(role);
        if !record
            .roles
            .iter()
            .zip(affected)
            .any(|(entry, changed)| changed && entry.touched)
        {
            return Ok(());
        }
        if record
            .roles
            .iter()
            .zip(affected)
            .any(|(entry, changed)| changed && !entry.owned)
        {
            return self.yield_group(record, affected);
        }
        // setter 可能改写整个组；每个成员均须回读，不能只检查代表角色。
        let mut all_original = true;
        for (index, changed) in affected.into_iter().enumerate() {
            if !changed {
                continue;
            }
            let current = self.backend.default_render(index)?;
            let entry = &record.roles[index];
            let original = current.eq_ignore_ascii_case(&entry.original);
            all_original &= original;
            if !original && !current.eq_ignore_ascii_case(&entry.installed) {
                return self.yield_group(record, affected);
            }
        }
        if all_original {
            for (entry, changed) in record.roles.iter_mut().zip(affected) {
                if changed {
                    entry.touched = false;
                }
            }
            return self.journal.save(record);
        }
        let original = record.roles[role].original.clone();
        if record
            .roles
            .iter()
            .zip(affected)
            .any(|(entry, changed)| changed && !entry.original.eq_ignore_ascii_case(&original))
        {
            return Err(format!("render role {role} restore failed: linked originals cannot be independently restored"));
        }
        for (entry, changed) in record.roles.iter_mut().zip(affected) {
            if changed {
                entry.touched = true;
            }
        }
        self.journal.save(record)?;
        self.backend
            .set_render(role, &original)
            .map_err(|error| format!("render role {role} restore failed: {error}"))?;
        for (index, changed) in affected.into_iter().enumerate() {
            if changed
                && !self
                    .backend
                    .default_render(index)?
                    .eq_ignore_ascii_case(&record.roles[index].original)
            {
                return Err(format!("render role {index} restore verification failed"));
            }
        }
        for (entry, changed) in record.roles.iter_mut().zip(affected) {
            if changed {
                entry.touched = false;
            }
        }
        self.journal.save(record)
    }

    fn yield_group(&mut self, record: &mut Record, affected: [bool; 3]) -> Result<(), String> {
        for (entry, changed) in record.roles.iter_mut().zip(affected) {
            if changed {
                entry.owned = false;
            }
        }
        self.released_external = true;
        self.journal.save(record)
    }
}
