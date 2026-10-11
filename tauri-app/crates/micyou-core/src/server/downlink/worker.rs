use super::*;

struct CaptureRun {
    connection: u64,
    session: i64,
    stream: i64,
    source: Option<String>,
    stop: Arc<AtomicBool>,
    cancel: CancellationToken,
    audio: mpsc::Sender<MessageWrapper>,
    runtime: tokio::runtime::Handle,
}

impl DownlinkController {
    pub(super) fn start_worker(self: &Arc<Self>) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let error = if !inner.status.supported {
            Some("unsupported_platform")
        } else if inner.thread.is_some() {
            Some("capture_failed")
        } else if inner.status.route_state == "restore_failed" {
            Some("route_restore_required")
        } else {
            None
        };
        if let Some(error) = error {
            inner.status.error = Some(error.into());
            self.publish(&inner);
            return;
        }
        let source = inner.status.source_id.clone();
        if Self::routing_enabled(&inner) && source.is_none() {
            inner.status.error = Some("route_source_required".into());
            self.publish(&inner);
            return;
        }
        let Some(session) = inner.session.as_mut() else {
            return;
        };
        let Some(stream) = session.stream_id.checked_add(1) else {
            session.cancel.cancel();
            return;
        };
        session.stream_id = stream;
        let stop = Arc::new(AtomicBool::new(false));
        let run = CaptureRun {
            connection: session.connection_id,
            session: session.session_id,
            stream,
            source,
            stop: stop.clone(),
            cancel: session.cancel.clone(),
            audio: session.audio.clone(),
            runtime: tokio::runtime::Handle::current(),
        };
        inner.status.active = false;
        inner.status.error = None;
        inner.stop = Some(stop);
        let controller = self.clone();
        match std::thread::Builder::new()
            .name("micyou-downlink".into())
            .spawn(move || {
                // panic和正常错误都交共享生命周期释放，不能只留下错误状态。
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    controller.capture_worker(&run)
                }));
                let error = match result {
                    Ok(Ok(())) => Some("capture_failed".into()),
                    Ok(Err(error)) if error == "cancelled" => None,
                    Ok(Err(error)) => Some(error),
                    Err(_) => Some("capture_failed".into()),
                };
                run.stop.store(true, Ordering::Release);
                run.runtime.spawn(async move {
                    controller
                        .finish_worker(run.connection, run.stream, error)
                        .await;
                });
            }) {
            Ok(thread) => inner.thread = Some(thread),
            Err(error) => {
                log::error!("[Downlink] spawn failed: {error}");
                inner.status.error = Some("capture_failed".into());
                self.publish(&inner);
            }
        }
    }

    fn capture_worker(self: &Arc<Self>, run: &CaptureRun) -> Result<(), String> {
        let endpoint = stereo_loopback::list_render_endpoints()?
            .into_iter()
            .find(|s| run.source.as_ref().map_or(s.is_default, |id| id == &s.id))
            .ok_or("source_unavailable")?;
        if Self::routing_enabled(&self.inner.lock().unwrap_or_else(|p| p.into_inner())) {
            let identity = (self.identity)(&endpoint.id)?;
            if identity.component != micyou_audio::endpoint::Component::Hifi {
                return Err("route_source_required".into());
            }
        }
        let mut sequence = 0i64;
        stereo_loopback::capture(
            &endpoint.id,
            run.stop.clone(),
            || {
                if run.stop.load(Ordering::Acquire) || run.cancel.is_cancelled() {
                    return Err("cancelled".into());
                }
                if let Some(error) = self.source_error(&endpoint.id) {
                    return Err(error.into());
                }
                if self.route.external_change() {
                    return Err(self.route.external_error());
                }
                if run.source.is_none()
                    && !micyou_audio::endpoint::default_render_endpoint_id()?
                        .eq_ignore_ascii_case(&endpoint.id)
                {
                    return Err("source_unavailable".into());
                }
                Ok(())
            },
            || {
                let controller = self.clone();
                let id = endpoint.id.clone();
                let (connection, stream) = (run.connection, run.stream);
                run.runtime.spawn(async move {
                    controller.worker_ready(connection, stream, id).await;
                });
            },
            |pcm| {
                let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
                if !inner.status.active
                    || !Self::generation_matches(&inner, run.connection, run.stream)
                {
                    return;
                }
                let message = MessageWrapper {
                    downlink_audio: Some(DownlinkAudioMessage {
                        session_id: run.session,
                        stream_id: run.stream,
                        sequence_number: sequence,
                        pcm,
                    }),
                    ..Default::default()
                };
                sequence = sequence.saturating_add(1);
                match run.audio.try_send(message) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        inner.status.dropped_frames = inner.status.dropped_frames.saturating_add(1);
                        if inner.status.dropped_frames % 50 == 1 {
                            self.events.downlink_status_changed(inner.status.clone());
                        }
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        run.stop.store(true, Ordering::Release);
                        run.cancel.cancel();
                    }
                }
            },
        )
    }

    pub(super) async fn worker_ready(self: &Arc<Self>, connection: u64, stream: i64, id: String) {
        let _gate = self.operation.lock().await;
        let routed = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if !Self::generation_matches(&inner, connection, stream) {
                return;
            }
            let routed = Self::routing_enabled(&inner);
            if routed {
                inner.status.route_state = "acquiring".into();
                inner.status.route_error = None;
                self.publish(&inner);
            }
            routed
        };
        if routed {
            if let Err(error) = self.acquire_route(id.clone()).await {
                self.stop_worker().await.ok();
                self.fail_current(error);
                return;
            }
        }
        let started = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            // 采集线程在取得路由期间也可能退出或连接取消。
            if Self::generation_matches(&inner, connection, stream)
                && inner
                    .session
                    .as_ref()
                    .is_some_and(|session| !session.cancel.is_cancelled())
            {
                inner.capture_source = Some(id);
                inner.status.active = true;
                inner.status.error = None;
                self.publish(&inner);
                true
            } else {
                false
            }
        };
        if !started {
            self.stop_worker().await.ok();
        }
    }

    pub(super) async fn finish_worker(&self, connection: u64, stream: i64, error: Option<String>) {
        let _gate = self.operation.lock().await;
        {
            let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            // stop已置位仍须匹配session/stream做最终释放；新代不能由旧worker释放。
            if !inner
                .session
                .as_ref()
                .is_some_and(|s| s.connection_id == connection && s.stream_id == stream)
            {
                return;
            }
        }
        let stopped = self.stop_worker().await;
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(error) = error {
            log::warn!("[Downlink] capture ended: {error}");
            inner.status.error = Some(match error.as_str() {
                "feedback_source"
                | "source_unavailable"
                | "identity_unavailable"
                | "route_changed_external"
                | "route_source_required"
                | "route_unavailable"
                | "route_restore_failed"
                | "transport_stalled" => error,
                _ => "capture_failed".into(),
            });
        } else if let Err(error) = stopped {
            inner.status.error = Some(error);
        }
        self.publish(&inner);
    }
}
