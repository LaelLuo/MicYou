/*
 * MicYou — Turns your Android device into a high-quality PC microphone.
 * Copyright (C) 2026 LanRhyme <https://github.com/MicYou-Dev/MicYou>
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version, with the MicYou Plugin Exception.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 * GNU General Public License for more details.
 */

//! Server state shared by the GUI, CLI and TUI, and the lifecycle
//! operations on it.

mod audio_pipeline;
pub mod downlink;
pub mod lifecycle;
pub mod output;
pub mod service;

use crate::events::SharedEvents;
use crate::host::SharedHost;
use crate::plugins::{PluginHost, ServerHandles};
use crate::stats::NetworkStats;
use crate::transport::tcp::{SharedActiveConnection, SharedTakeoverLock};
use crate::transport::udp::{ActiveAudioSession, SharedActiveAudioSession};
use lifecycle::{ServerLifecycleGate, ServerLifecycleState};
use micyou_audio::dsp::AudioDspSettings;
use output::AudioOutputHandle;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub use service::{start_server, stop_server, StartRequest};

pub struct ServerState {
    output_selection_gate: std::sync::Mutex<()>,
    pub audio_routing: Arc<RwLock<crate::audio_routing::AudioRoutingStatus>>,
    pub downlink: Arc<downlink::DownlinkController>,
    pub events: SharedEvents,
    /// The frontend's own resource directory, if it knows one (see
    /// [`crate::platform::resources::find_resource_dir`]).
    pub resource_hint: Option<PathBuf>,
    pub lifecycle_gate: ServerLifecycleGate,
    pub lifecycle: Arc<Mutex<ServerLifecycleState>>,
    pub cancel_token: Arc<Mutex<Option<CancellationToken>>>,
    pub background_tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    pub mdns_manager: Arc<Mutex<Option<crate::discovery::NetworkManager>>>,
    pub dsp_settings: Arc<RwLock<AudioDspSettings>>,
    pub is_monitoring: Arc<AtomicBool>,
    pub spectrum_streaming_enabled: Arc<AtomicBool>,
    pub network_stats: Arc<NetworkStats>,
    pub active_connection: SharedActiveConnection,
    pub takeover_lock: SharedTakeoverLock,
    pub active_audio_session: SharedActiveAudioSession,
    pub audio_output: Arc<AudioOutputHandle>,
    pub plugins: Arc<PluginHost>,
    #[cfg(feature = "web-server")]
    pub web_server: Arc<Mutex<Option<crate::transport::web::WebServer>>>,
    #[cfg(feature = "web-server")]
    pub web_mdns: Arc<Mutex<Option<crate::discovery::NetworkManager>>>,
}

impl ServerState {
    /// Build the state with DSP settings loaded from the shared settings.json.
    pub fn new(events: SharedEvents, host: SharedHost, resource_hint: Option<PathBuf>) -> Self {
        let network_stats = Arc::new(NetworkStats::default());
        // The output engine watches the stats' mute flag directly: switching to
        // muted drops queued audio and outputs silence within one callback period.
        let audio_output = AudioOutputHandle::spawn_with_mute_flag(network_stats.mute_flag());
        let downlink = downlink::DownlinkController::new(
            events.clone(),
            vec![audio_output.actual_endpoint_id.clone()],
        );
        let active_connection = Arc::new(Mutex::new(None));
        let active_audio_session = Arc::new(RwLock::new(ActiveAudioSession::Inactive));
        let lifecycle = Arc::new(Mutex::new(ServerLifecycleState::default()));
        #[cfg(feature = "web-server")]
        let web_server = Arc::new(Mutex::new(None));

        let plugins = PluginHost::new(
            ServerHandles {
                network_stats: network_stats.clone(),
                audio_output: audio_output.clone(),
                active_connection: active_connection.clone(),
                active_audio_session: active_audio_session.clone(),
                lifecycle: lifecycle.clone(),
                #[cfg(feature = "web-server")]
                web_server: web_server.clone(),
            },
            host,
        );

        let state = Self {
            output_selection_gate: std::sync::Mutex::new(()),
            audio_routing: Arc::new(RwLock::new(
                crate::audio_routing::AudioRoutingStatus::default(),
            )),
            downlink,
            events,
            resource_hint,
            lifecycle_gate: ServerLifecycleGate::default(),
            lifecycle,
            cancel_token: Arc::new(Mutex::new(None)),
            background_tasks: Arc::new(Mutex::new(Vec::new())),
            mdns_manager: Arc::new(Mutex::new(None)),
            dsp_settings: Arc::new(RwLock::new(crate::config::load_dsp_settings())),
            is_monitoring: Arc::new(AtomicBool::new(false)),
            spectrum_streaming_enabled: Arc::new(AtomicBool::new(false)),
            network_stats,
            active_connection,
            takeover_lock: Arc::new(Mutex::new(())),
            active_audio_session,
            audio_output,
            plugins,
            #[cfg(feature = "web-server")]
            web_server,
            #[cfg(feature = "web-server")]
            web_mdns: Arc::new(Mutex::new(None)),
        };
        state.plugins.attach_controls(state.controls());
        state
    }

    /// Resolved directory of bundled models and configs.
    pub fn resource_dir(&self) -> Option<PathBuf> {
        crate::platform::resources::find_resource_dir(self.resource_hint.as_deref())
    }

    pub fn get_audio_routing_status(&self) -> crate::audio_routing::AudioRoutingStatus {
        let mut status = self
            .audio_routing
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        status.output_endpoint_id = self
            .audio_output
            .actual_endpoint_id
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        status.monitor_endpoint_id = self
            .audio_output
            .monitor_endpoint_id
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        status
    }

    pub fn set_output_endpoint(
        &self,
        id: String,
    ) -> Result<crate::audio_routing::AudioRoutingStatus, String> {
        let _selection = self
            .output_selection_gate
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if id.is_empty() {
            return Err("output_endpoint_unavailable".into());
        }
        let endpoints = micyou_audio::endpoint::list_output_endpoints()?;
        let selected = crate::audio_routing::resolve_endpoint(&endpoints, Some(&id), "")?;
        let buffer = self.current_dsp_settings().output_buffer_ms as usize;
        if !self
            .audio_output
            .ensure_open(Some(selected.id.clone()), buffer.clamp(100, 1200))
        {
            self.audio_routing
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .error = Some("output_open_failed".into());
            self.events
                .audio_routing_status_changed(self.get_audio_routing_status());
            return Err("output_open_failed".into());
        }
        if let Err(error) = crate::config::patch_server_prefs(crate::config::ServerPrefsPatch {
            output_endpoint_id: Some(selected.id.clone()),
            output_device: Some(selected.name.clone()),
            ..Default::default()
        }) {
            self.audio_routing
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .error = Some("output_persistence_failed".into());
            self.events
                .audio_routing_status_changed(self.get_audio_routing_status());
            return Err(error);
        }
        self.audio_routing
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .error = None;
        let status = self.get_audio_routing_status();
        self.events.audio_routing_status_changed(status.clone());
        Ok(status)
    }

    pub async fn list_downlink_sources(&self) -> Result<Vec<downlink::DownlinkSource>, String> {
        let controller = self.downlink.clone();
        tokio::task::spawn_blocking(move || controller.sources())
            .await
            .map_err(|e| e.to_string())?
    }
    pub fn get_downlink_status(&self) -> downlink::DownlinkStatus {
        self.downlink.status()
    }
    pub async fn set_downlink_source(
        &self,
        source_id: Option<String>,
    ) -> Result<downlink::DownlinkStatus, String> {
        self.downlink.set_source(source_id).await
    }
}
