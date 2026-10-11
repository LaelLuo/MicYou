use super::*;
use std::sync::{Arc, Mutex};

struct Fake {
    defaults: [String; 3],
    occupied: bool,
    held: bool,
    record: Option<Record>,
    live_owner: Option<Owner>,
    load_error: bool,
    save_count: usize,
    fail_save: Option<usize>,
    fail_set: Option<(usize, String, bool)>,
    fail_remove: bool,
    changes: [bool; 3],
    fail_read_after_set: Option<(usize, String)>,
    next_read_error: Option<usize>,
    missing_role: [bool; 3],
    invalidated: Vec<String>,
    #[cfg(windows)]
    notifications: crate::playback_route::native::Notifications,
    #[cfg(windows)]
    late_notifications: Vec<(usize, String)>,
    actions: Vec<String>,
}

impl Default for Fake {
    fn default() -> Self {
        Self {
            defaults: [
                "speakers".into(),
                "headphones".into(),
                "communications".into(),
            ],
            occupied: false,
            held: false,
            record: None,
            live_owner: None,
            load_error: false,
            save_count: 0,
            fail_save: None,
            fail_set: None,
            fail_remove: false,
            changes: [false; 3],
            actions: Vec::new(),
            fail_read_after_set: None,
            next_read_error: None,
            missing_role: [false; 3],
            invalidated: Vec::new(),
            #[cfg(windows)]
            notifications: Default::default(),
            #[cfg(windows)]
            late_notifications: Vec::new(),
        }
    }
}
type Shared = Arc<Mutex<Fake>>;
struct FakeBackend(Shared);
struct FakeJournal(Shared);
fn owner() -> Owner {
    Owner {
        pid: 42,
        created: 100,
        instance: "instance".into(),
    }
}
fn machine() -> (LeaseMachine<FakeBackend, FakeJournal>, Shared) {
    let fake = Arc::new(Mutex::new(Fake::default()));
    (
        LeaseMachine::new(FakeBackend(fake.clone()), FakeJournal(fake.clone())),
        fake,
    )
}

impl Backend for FakeBackend {
    fn try_lock(&mut self) -> Result<(), String> {
        let mut fake = self.0.lock().unwrap();
        fake.actions.push("lock".into());
        if fake.occupied {
            return Err("route_busy".into());
        }
        fake.held = true;
        Ok(())
    }
    fn unlock(&mut self) {
        let mut fake = self.0.lock().unwrap();
        fake.held = false;
        fake.actions.push("unlock".into());
    }
    fn owner(&self) -> Owner {
        owner()
    }
    fn owner_alive(&self, owner: &Owner) -> Result<bool, String> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .live_owner
            .as_ref()
            .is_some_and(|live| live.pid == owner.pid && live.created == owner.created))
    }
    fn defaults(&self) -> Result<[String; 3], String> {
        let mut fake = self.0.lock().unwrap();
        assert!(fake.held);
        fake.actions.push("read-defaults".into());
        Ok(fake.defaults.clone())
    }
    fn default_render(&self, role: usize) -> Result<String, String> {
        let mut fake = self.0.lock().unwrap();
        if fake.next_read_error == Some(role) {
            fake.next_read_error = None;
            return Err("fake readback failure".into());
        }
        if fake.missing_role[role] {
            return Err("fake endpoint missing".into());
        }
        Ok(fake.defaults[role].clone())
    }
    fn set_render(&mut self, role: usize, endpoint: &str) -> Result<(), String> {
        let mut fake = self.0.lock().unwrap();
        assert!(fake.held, "never write without cross-process lock");
        assert!(
            fake.record.as_ref().unwrap().roles[role].touched,
            "journal must precede setter"
        );
        fake.actions.push(format!("set:{role}:{endpoint}"));
        #[cfg(windows)]
        fake.notifications.expected[role].push_back(endpoint.into());
        let fail = fake
            .fail_set
            .as_ref()
            .is_some_and(|(r, id, _)| *r == role && id == endpoint);
        if fail {
            let (_, _, mutate) = fake.fail_set.take().unwrap();
            if mutate {
                fake.defaults[role] = endpoint.into();
            }
            #[cfg(windows)]
            {
                fake.notifications.failed_write(role, endpoint, mutate);
                if mutate {
                    fake.late_notifications.push((role, endpoint.into()));
                }
            }
            return Err("fake setter failure".into());
        }
        fake.defaults[role] = endpoint.into();
        #[cfg(windows)]
        fake.late_notifications.push((role, endpoint.into()));
        if fake
            .fail_read_after_set
            .as_ref()
            .is_some_and(|(r, id)| *r == role && id == endpoint)
        {
            fake.fail_read_after_set = None;
            fake.next_read_error = Some(role);
        }
        Ok(())
    }
    fn external_changes(&mut self) -> [bool; 3] {
        let mut fake = self.0.lock().unwrap();
        #[cfg(windows)]
        {
            for (role, endpoint) in std::mem::take(&mut fake.late_notifications) {
                fake.notifications.observed(role, endpoint);
            }
            let external = std::mem::take(&mut fake.notifications.external);
            for (role, changed) in external.into_iter().enumerate() {
                fake.changes[role] |= changed;
            }
        }
        std::mem::take(&mut fake.changes)
    }
    fn reset_notifications(&mut self) {
        self.0.lock().unwrap().changes = [false; 3];
    }
    fn invalidated_endpoints(&mut self) -> Vec<String> {
        std::mem::take(&mut self.0.lock().unwrap().invalidated)
    }
}
impl Journal for FakeJournal {
    fn load(&self) -> Result<Option<Record>, String> {
        let mut fake = self.0.lock().unwrap();
        assert!(fake.held);
        fake.actions.push("load-journal".into());
        if fake.load_error {
            return Err("fake invalid journal".into());
        }
        Ok(fake.record.clone())
    }
    fn save(&mut self, record: &Record) -> Result<(), String> {
        let mut fake = self.0.lock().unwrap();
        assert!(fake.held);
        fake.save_count += 1;
        fake.actions.push("save-journal".into());
        if fake.fail_save == Some(fake.save_count) {
            fake.fail_save = None;
            return Err("fake disk failure".into());
        }
        fake.record = Some(record.clone());
        Ok(())
    }
    fn remove(&mut self) -> Result<(), String> {
        let mut fake = self.0.lock().unwrap();
        assert!(fake.held);
        fake.actions.push("remove-journal".into());
        if fake.fail_remove {
            return Err("fake delete failure".into());
        }
        fake.record = None;
        Ok(())
    }
}

#[test]
fn denied_lock_never_reads_or_writes_defaults_or_journal() {
    let (mut route, fake) = machine();
    fake.lock().unwrap().occupied = true;
    assert!(route.acquire("hifi").is_err());
    assert_eq!(fake.lock().unwrap().actions, ["lock"]);
}

#[test]
fn distinct_role_snapshot_roundtrips_and_repeated_acquire_keeps_originals() {
    let (mut route, fake) = machine();
    let original = fake.lock().unwrap().defaults.clone();
    assert_eq!(route.acquire("hifi").unwrap().state, RouteState::Held);
    let count = fake.lock().unwrap().actions.len();
    route.acquire("hifi").unwrap();
    assert_eq!(fake.lock().unwrap().actions.len(), count);
    assert!(route.acquire("other").is_err());
    assert_eq!(route.release().state, RouteState::Restored);
    assert_eq!(route.release().state, RouteState::Restored);
    let fake = fake.lock().unwrap();
    assert_eq!(fake.defaults, original);
    assert!(fake.record.is_none());
    assert!(!fake.held);
}

#[test]
fn partial_setter_failure_rolls_back_only_touched_installed_roles() {
    for mutate in [false, true] {
        let (mut route, fake) = machine();
        let original = fake.lock().unwrap().defaults.clone();
        fake.lock().unwrap().fail_set = Some((1, "hifi".into(), mutate));
        assert!(route.acquire("hifi").is_err());
        assert_eq!(route.status().state, RouteState::Restored);
        let fake = fake.lock().unwrap();
        assert_eq!(fake.defaults, original);
        assert!(!fake
            .actions
            .iter()
            .any(|action| action.starts_with("set:2:")));
        assert!(fake.record.is_none());
    }
}

#[test]
fn disk_failure_before_any_setter_leaves_defaults_alone() {
    let (mut route, fake) = machine();
    fake.lock().unwrap().fail_save = Some(1);
    assert!(route.acquire("hifi").is_err());
    let fake = fake.lock().unwrap();
    assert!(!fake.actions.iter().any(|action| action.starts_with("set:")));
    assert!(fake.record.is_none());
    assert!(!fake.held);
}

#[test]
fn disk_failure_after_partial_switch_recovers_saved_originals() {
    let (mut route, fake) = machine();
    let original = fake.lock().unwrap().defaults.clone();
    fake.lock().unwrap().fail_save = Some(3);
    assert!(route.acquire("hifi").is_err());
    assert_eq!(route.status().state, RouteState::Restored);
    assert_eq!(fake.lock().unwrap().defaults, original);
}

#[test]
fn restore_failure_preserves_record_and_lock_until_explicit_retry() {
    let (mut route, fake) = machine();
    route.acquire("hifi").unwrap();
    fake.lock().unwrap().fail_set = Some((0, "speakers".into(), false));
    assert_eq!(route.release().state, RouteState::RestoreFailed);
    let count = fake.lock().unwrap().actions.len();
    assert!(route.acquire("hifi").is_err());
    assert_eq!(fake.lock().unwrap().actions.len(), count);
    assert!(fake.lock().unwrap().record.is_some());
    assert!(fake.lock().unwrap().held);
    assert_eq!(route.release().state, RouteState::Restored);
    assert!(fake.lock().unwrap().record.is_none());
}

#[test]
fn external_out_and_back_relinquishes_role_before_core_stops_pcm() {
    let (mut route, fake) = machine();
    route.acquire("hifi").unwrap();
    fake.lock().unwrap().changes[0] = true;
    let sets_before = fake
        .lock()
        .unwrap()
        .actions
        .iter()
        .filter(|a| a.starts_with("set:"))
        .count();
    assert!(route.external_change());
    let sets_after = fake
        .lock()
        .unwrap()
        .actions
        .iter()
        .filter(|a| a.starts_with("set:"))
        .count();
    assert_eq!(
        sets_before, sets_after,
        "notification check must not restore before PCM stop"
    );
    assert_eq!(route.release().state, RouteState::ReleasedExternal);
    let fake = fake.lock().unwrap();
    assert_eq!(fake.defaults, ["hifi", "headphones", "communications"]);
}

#[test]
fn missing_recovery_baseline_is_rejected_without_creating_record() {
    let (mut route, fake) = machine();
    fake.lock().unwrap().defaults[2] = "hifi".into();
    assert!(route
        .acquire("hifi")
        .unwrap_err()
        .starts_with("route_restore_target_required"));
    assert!(fake.lock().unwrap().record.is_none());
}

#[test]
fn live_owner_is_never_restored_or_overwritten() {
    let (mut old, fake) = machine();
    old.acquire("hifi").unwrap();
    fake.lock().unwrap().live_owner = Some(owner());
    let mut next = LeaseMachine::new(FakeBackend(fake.clone()), FakeJournal(fake.clone()));
    let sets_before = fake
        .lock()
        .unwrap()
        .actions
        .iter()
        .filter(|a| a.starts_with("set:"))
        .count();
    assert_eq!(next.recover_stale().state, RouteState::RestoreFailed);
    assert_eq!(
        fake.lock()
            .unwrap()
            .actions
            .iter()
            .filter(|a| a.starts_with("set:"))
            .count(),
        sets_before
    );
    assert_eq!(fake.lock().unwrap().defaults, ["hifi", "hifi", "hifi"]);
}

#[test]
fn stale_owner_and_reused_pid_restore_journal_from_previous_instance() {
    let (mut old, fake) = machine();
    old.acquire("hifi").unwrap();
    fake.lock().unwrap().live_owner = Some(Owner {
        created: 200,
        ..owner()
    });
    let mut next = LeaseMachine::new(FakeBackend(fake.clone()), FakeJournal(fake.clone()));
    assert_eq!(next.recover_stale().state, RouteState::Restored);
    assert_eq!(
        fake.lock().unwrap().defaults,
        ["speakers", "headphones", "communications"]
    );
    assert!(fake.lock().unwrap().record.is_none());
}

#[test]
fn invalid_journal_blocks_acquire_until_explicit_recovery() {
    let (mut route, fake) = machine();
    fake.lock().unwrap().load_error = true;
    assert_eq!(route.recover_stale().state, RouteState::RestoreFailed);
    fake.lock().unwrap().load_error = false;
    assert!(route.acquire("hifi").is_err());
    assert_eq!(route.release().state, RouteState::Idle);
    assert_eq!(route.acquire("hifi").unwrap().state, RouteState::Held);
}

#[test]
fn journal_delete_failure_can_retry_without_reapplying_restored_roles() {
    let (mut route, fake) = machine();
    route.acquire("hifi").unwrap();
    fake.lock().unwrap().fail_remove = true;
    assert_eq!(route.release().state, RouteState::RestoreFailed);
    let count = fake
        .lock()
        .unwrap()
        .actions
        .iter()
        .filter(|a| a.starts_with("set:"))
        .count();
    fake.lock().unwrap().fail_remove = false;
    assert_eq!(route.release().state, RouteState::Restored);
    assert_eq!(
        fake.lock()
            .unwrap()
            .actions
            .iter()
            .filter(|a| a.starts_with("set:"))
            .count(),
        count
    );
}

#[test]
fn readback_failure_after_write_still_rolls_back_the_installed_target() {
    let (mut route, fake) = machine();
    fake.lock().unwrap().fail_read_after_set = Some((1, "hifi".into()));
    assert!(route.acquire("hifi").is_err());
    assert_eq!(route.status().state, RouteState::Restored);
    assert_eq!(
        fake.lock().unwrap().defaults,
        ["speakers", "headphones", "communications"]
    );
    assert!(fake.lock().unwrap().record.is_none());
}

#[test]
fn missing_role_does_not_block_conditional_restore_of_other_roles() {
    let (mut route, fake) = machine();
    route.acquire("hifi").unwrap();
    fake.lock().unwrap().missing_role[0] = true;
    assert_eq!(route.release().state, RouteState::RestoreFailed);
    assert_eq!(
        fake.lock().unwrap().defaults,
        ["hifi", "headphones", "communications"]
    );
    fake.lock().unwrap().missing_role[0] = false;
    assert_eq!(route.release().state, RouteState::Restored);
}

#[test]
fn removed_original_stops_pcm_without_yielding_owned_role_and_preserves_failed_restore() {
    let (mut route, fake) = machine();
    route.acquire("hifi").unwrap();
    {
        let mut fake = fake.lock().unwrap();
        fake.invalidated.push("speakers".into());
        fake.fail_set = Some((0, "speakers".into(), false));
    }
    assert!(route.external_change());
    assert!(fake.lock().unwrap().record.as_ref().unwrap().roles[0].owned);
    assert_eq!(
        route.status().error.as_deref(),
        Some("route_device_unavailable")
    );
    assert!(route.acquire("hifi").is_err());
    assert_eq!(route.release().state, RouteState::RestoreFailed);
    assert!(fake.lock().unwrap().record.is_some());
    assert_eq!(route.release().state, RouteState::Restored);
}

#[test]
fn unrelated_device_removed_does_not_stop_current_route() {
    let (mut route, fake) = machine();
    route.acquire("hifi").unwrap();
    fake.lock().unwrap().invalidated.push("unrelated".into());
    assert!(!route.external_change());
    assert_eq!(route.status().state, RouteState::Held);
}
