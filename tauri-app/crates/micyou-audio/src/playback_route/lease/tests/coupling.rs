use super::*;

fn linked_machine() -> (LeaseMachine<FakeBackend, FakeJournal>, Shared) {
    let (machine, shared) = machine();
    {
        let mut fake = shared.lock().unwrap();
        fake.linked = true;
        fake.defaults[1] = "speakers".into();
    }
    (machine, shared)
}

#[test]
fn linked_pair_and_distinct_communications_restore() {
    let (mut machine, shared) = linked_machine();
    assert!(machine.acquire("hifi").is_ok());
    assert!(!machine.external_change());
    assert_eq!(machine.release().state, RouteState::Restored);
    let fake = shared.lock().unwrap();
    assert_eq!(fake.defaults, ["speakers", "speakers", "communications"]);
    assert!(!fake
        .actions
        .iter()
        .any(|action| action.starts_with("set:1:")));
}

#[test]
fn differing_linked_originals_rejected_before_journal_or_setter() {
    let (mut machine, shared) = machine();
    shared.lock().unwrap().linked = true;
    assert!(machine
        .acquire("hifi")
        .unwrap_err()
        .starts_with("route_restore_target_required"));
    let fake = shared.lock().unwrap();
    assert_eq!(fake.save_count, 0);
    assert!(!fake.actions.iter().any(|action| action.starts_with("set:")));
}

#[test]
fn partial_pair_failure_restores_and_reacquires_without_phantom_notification() {
    let (mut machine, shared) = linked_machine();
    {
        let mut fake = shared.lock().unwrap();
        fake.partial = true;
        fake.fail_set = Some((0, "hifi".into(), true));
    }
    assert!(machine.acquire("hifi").is_err());
    assert_eq!(
        shared.lock().unwrap().defaults,
        ["speakers", "speakers", "communications"]
    );
    assert!(machine.acquire("hifi").is_ok());
    assert!(!machine.external_change());
    assert_eq!(machine.release().state, RouteState::Restored);
}

#[test]
fn communication_failure_rolls_back_entire_installed_pair() {
    let (mut machine, shared) = linked_machine();
    shared.lock().unwrap().fail_set = Some((2, "hifi".into(), false));
    assert!(machine.acquire("hifi").is_err());
    assert_eq!(
        shared.lock().unwrap().defaults,
        ["speakers", "speakers", "communications"]
    );
}

#[test]
fn silent_peer_override_yields_pair_but_restores_communications() {
    let (mut machine, shared) = linked_machine();
    machine.acquire("hifi").unwrap();
    assert!(!machine.external_change());
    shared.lock().unwrap().defaults[1] = "manual".into();
    assert_eq!(machine.release().state, RouteState::ReleasedExternal);
    assert_eq!(
        shared.lock().unwrap().defaults,
        ["hifi", "manual", "communications"]
    );
}

#[test]
fn pair_out_and_back_notification_latches_external_ownership() {
    let (mut machine, shared) = linked_machine();
    machine.acquire("hifi").unwrap();
    assert!(!machine.external_change());
    shared.lock().unwrap().changes[1] = true;
    assert!(machine.external_change());
    assert_eq!(machine.release().state, RouteState::ReleasedExternal);
    assert_eq!(
        shared.lock().unwrap().defaults,
        ["hifi", "hifi", "communications"]
    );
}

#[test]
fn missing_pair_default_does_not_block_independent_communication_restore() {
    let (mut machine, shared) = linked_machine();
    machine.acquire("hifi").unwrap();
    shared.lock().unwrap().missing_role[0] = true;
    assert_eq!(machine.release().state, RouteState::RestoreFailed);
    assert_eq!(shared.lock().unwrap().defaults[2], "communications");
}
