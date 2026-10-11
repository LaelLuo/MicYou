use super::*;
use crate::platform::playback_route::{PlaybackRouteController, RouteOutcome, RouteState};

pub(super) trait RouteLease: Send + Sync {
    fn supported(&self) -> bool;
    fn acquire(&self, target: &str) -> Result<RouteOutcome, String>;
    fn release(&self) -> RouteOutcome;
    fn external_change(&self) -> bool;
    fn external_error(&self) -> String {
        "route_changed_external".into()
    }
    fn recover_stale(&self) -> RouteOutcome;
}

impl RouteLease for PlaybackRouteController {
    fn supported(&self) -> bool {
        PlaybackRouteController::supported()
    }
    fn acquire(&self, target: &str) -> Result<RouteOutcome, String> {
        self.acquire(target)
    }
    fn release(&self) -> RouteOutcome {
        self.release()
    }
    fn external_change(&self) -> bool {
        self.external_change()
    }
    fn external_error(&self) -> String {
        external_error_for_outcome(&self.status()).into()
    }
    fn recover_stale(&self) -> RouteOutcome {
        self.recover_stale()
    }
}

fn external_error_for_outcome(outcome: &RouteOutcome) -> &'static str {
    // actor通道失败也会触发停止，但它不是用户修改默认输出。
    if outcome.error.as_deref() == Some("route_device_unavailable") {
        "source_unavailable"
    } else if outcome.state == RouteState::RestoreFailed {
        if outcome
            .error
            .as_deref()
            .is_some_and(|error| error.starts_with("route_restore_failed"))
        {
            "route_restore_failed"
        } else {
            "route_unavailable"
        }
    } else {
        "route_changed_external"
    }
}

struct UnavailableLease(Option<String>);
impl RouteLease for UnavailableLease {
    fn supported(&self) -> bool {
        false
    }
    fn acquire(&self, _: &str) -> Result<RouteOutcome, String> {
        Err(self
            .0
            .clone()
            .unwrap_or_else(|| "unsupported_platform".into()))
    }
    fn release(&self) -> RouteOutcome {
        RouteOutcome {
            state: RouteState::Idle,
            error: None,
        }
    }
    fn external_change(&self) -> bool {
        false
    }
    fn recover_stale(&self) -> RouteOutcome {
        self.release()
    }
}

pub(super) fn new_lease() -> Arc<dyn RouteLease> {
    // core单测不创建原生actor、不读写真实默认或恢复日志。
    #[cfg(test)]
    {
        Arc::new(UnavailableLease(None))
    }
    #[cfg(not(test))]
    {
        match PlaybackRouteController::new() {
            Ok(route) => Arc::new(route),
            Err(error) => {
                log::warn!("[Downlink] Automatic playback routing unavailable: {error}");
                Arc::new(UnavailableLease(Some(error)))
            }
        }
    }
}

pub(super) fn playback_error(error: &str) -> Option<String> {
    match error {
        "" => None,
        "playback_failed"
        | "invalid_frame"
        | "builtin_microphone_required"
        | "unsupported_android"
        | "unsupported_peer" => Some(error.into()),
        _ => Some("playback_failed".into()),
    }
}

impl DownlinkController {
    pub(super) fn routing_enabled(inner: &Inner) -> bool {
        inner.status.supports_playback_routing
            && inner.session.as_ref().is_some_and(|s| s.supports_routing)
    }

    fn record_route(&self, outcome: RouteOutcome) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.status.route_state = outcome.state.as_str().into();
        inner.status.route_error = outcome.error;
    }

    pub(super) async fn acquire_route(&self, id: String) -> Result<(), String> {
        let route = self.route.clone();
        let result = tokio::task::spawn_blocking(move || route.acquire(&id))
            .await
            .map_err(|_| "route_acquire_failed".to_string())?;
        match result {
            Ok(outcome) => {
                let held = outcome.state == RouteState::Held;
                let error = outcome
                    .error
                    .clone()
                    .unwrap_or_else(|| "route_acquire_failed".into());
                self.record_route(outcome);
                if held {
                    Ok(())
                } else {
                    Err(error)
                }
            }
            Err(error) => {
                let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
                inner.status.route_error = Some(error.clone());
                Err(error)
            }
        }
    }

    pub(super) async fn release_route(&self) {
        let route = self.route.clone();
        let outcome = tokio::task::spawn_blocking(move || route.release())
            .await
            .unwrap_or_else(|_| RouteOutcome {
                state: RouteState::RestoreFailed,
                error: Some("route_restore_failed".into()),
            });
        self.record_route(outcome);
    }

    pub async fn recover_route(&self) {
        let _gate = self.operation.lock().await;
        // GUI启动恢复任务可能晚于用户连接；不得把当前会话误当遗留事务。
        if self
            .inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .session
            .is_some()
        {
            return;
        }
        let route = self.route.clone();
        let outcome = tokio::task::spawn_blocking(move || route.recover_stale())
            .await
            .unwrap_or_else(|_| RouteOutcome {
                state: RouteState::RestoreFailed,
                error: Some("route_restore_failed".into()),
            });
        self.record_route(outcome);
        self.publish(&self.inner.lock().unwrap_or_else(|p| p.into_inner()));
    }

    pub async fn retry_route_restore(&self) -> DownlinkStatus {
        let _gate = self.operation.lock().await;
        if self.status().route_state == "restore_failed" {
            self.release_route().await;
            self.publish(&self.inner.lock().unwrap_or_else(|p| p.into_inner()));
        }
        self.status()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_failure_is_not_reported_as_a_manual_default_change() {
        let outcome = RouteOutcome {
            state: RouteState::RestoreFailed,
            error: Some("routing actor unavailable".into()),
        };
        assert_eq!(external_error_for_outcome(&outcome), "route_unavailable");
        let outcome = RouteOutcome {
            state: RouteState::RestoreFailed,
            error: Some("route_restore_failed: journal write failed".into()),
        };
        assert_eq!(external_error_for_outcome(&outcome), "route_restore_failed");
    }

    #[test]
    fn genuine_external_change_and_missing_device_keep_distinct_causes() {
        let mut outcome = RouteOutcome {
            state: RouteState::Held,
            error: None,
        };
        assert_eq!(
            external_error_for_outcome(&outcome),
            "route_changed_external"
        );
        outcome.error = Some("route_device_unavailable".into());
        assert_eq!(external_error_for_outcome(&outcome), "source_unavailable");
    }

    #[test]
    fn unavailable_routing_does_not_report_a_failed_lease_to_manual_downlink() {
        let lease = UnavailableLease(Some("COM initialization failed".into()));
        assert!(!lease.supported());
        assert_eq!(lease.release().state, RouteState::Idle);
        assert!(lease.release().error.is_none());
        assert!(lease.recover_stale().error.is_none());
    }
}
