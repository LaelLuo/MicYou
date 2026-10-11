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

use micyou_audio::dsp::AudioDspSettings;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Shared config directory (Windows: %APPDATA%\micyou, unix: XDG_CONFIG_HOME or ~/.config + micyou).
pub fn config_dir() -> PathBuf {
    if let Some(dir) = config_override_dir() {
        return dir;
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("micyou");
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let dir = PathBuf::from(xdg).join("micyou");
        if !dir.as_os_str().is_empty() {
            return dir;
        }
    }
    std::env::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("micyou")
}

/// Isolate a portable instance's configuration and process lock together.
pub(crate) fn config_override_dir() -> Option<PathBuf> {
    std::env::var_os("MICYOU_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// settings.json: the DSP settings shared by GUI, CLI and TUI.
pub fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

/// ui.json: GUI UI preferences (language, theme color) that the TUI reads.
pub fn ui_prefs_path() -> PathBuf {
    config_dir().join("ui.json")
}

/// theme.json: current GUI theme colors exported for the TUI.
pub fn theme_path() -> PathBuf {
    config_dir().join("theme.json")
}

/// server.json: connection-level settings shared by GUI, CLI and TUI
/// (port, mode, bind address, output device).
pub fn server_prefs_path() -> PathBuf {
    config_dir().join("server.json")
}

fn load_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text)
        .inspect_err(|e| log::warn!("[Config] ignoring malformed {}: {e}", path.display()))
        .ok()
}

/// Write a config file atomically: the GUI, CLI and TUI read these files
/// concurrently, and a half-written file would parse as defaults.
fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(|e| format!("create config dir failed: {e}"))?;
    let json =
        serde_json::to_string_pretty(value).map_err(|e| format!("serialize {name} failed: {e}"))?;
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    fs::write(&tmp, json)
        .and_then(|()| fs::rename(&tmp, path))
        .inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })
        .map_err(|e| format!("write {name} failed: {e}"))
}

/// Load DSP settings from settings.json, falling back to defaults.
pub fn load_dsp_settings() -> AudioDspSettings {
    load_json::<AudioDspSettings>(&settings_path())
        .map(|mut settings| {
            settings.normalize();
            settings
        })
        .unwrap_or_default()
}

/// Persist DSP settings to settings.json (GUI, CLI and TUI share this file).
pub fn save_dsp_settings(settings: &AudioDspSettings) -> Result<(), String> {
    let mut normalized = settings.clone();
    normalized.normalize();
    save_json(&settings_path(), &normalized)
}

/// Raw settings.json as a JSON value (for the CLI `settings get`).
pub fn settings_json() -> serde_json::Value {
    load_json(&settings_path())
        .unwrap_or_else(|| serde_json::to_value(AudioDspSettings::default()).unwrap_or_default())
}

/// GUI UI preferences persisted to ui.json.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct UiPrefs {
    pub language: String,
    pub theme_color: String,
}

pub fn load_ui_prefs() -> UiPrefs {
    load_json(&ui_prefs_path()).unwrap_or_default()
}

pub fn save_ui_prefs(prefs: &UiPrefs) -> Result<(), String> {
    save_json(&ui_prefs_path(), prefs)
}

/// Theme colors exported from the GUI for the TUI.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ThemeColors {
    pub primary: String,
    pub secondary: String,
    pub tertiary: String,
    pub surface: String,
    pub surface_variant: String,
    pub on_surface: String,
    pub error: String,
}

pub fn load_theme_colors() -> ThemeColors {
    load_json(&theme_path()).unwrap_or_default()
}

pub fn save_theme_colors(colors: &ThemeColors) -> Result<(), String> {
    save_json(&theme_path(), colors)
}

/// Connection-level settings shared between the GUI, CLI and TUI.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct ServerPrefs {
    /// Streaming port for wifi/usb modes.
    pub port: u16,
    /// Port for the web (https) mode.
    pub web_port: u16,
    /// Connection mode: wifi | usb | web.
    pub mode: String,
    /// Bind address ("0.0.0.0" when auto-bind).
    pub bind_address: String,
    /// Whether to listen on all interfaces.
    pub auto_bind: bool,
    /// Selected output audio device name.
    pub output_device: String,
    /// Authoritative Windows endpoint identity; names are migration/display only.
    pub output_endpoint_id: Option<String>,
    /// 独立回环来源；None 表示跟随默认播放端点，开启状态不落盘。
    pub downlink_source_id: Option<String>,
    /// Whether mute state is synchronized with the mobile client in both
    /// directions. When false, the desktop neither sends its mute state to
    /// the phone nor applies mute state received from it. Defaults to true,
    /// including for server.json files written before this field existed.
    pub mute_sync: bool,
}

impl Default for ServerPrefs {
    fn default() -> Self {
        Self {
            port: 8554,
            web_port: 8443,
            mode: "wifi".to_string(),
            bind_address: "0.0.0.0".to_string(),
            auto_bind: true,
            output_device: String::new(),
            output_endpoint_id: None,
            downlink_source_id: None,
            mute_sync: true,
        }
    }
}

/// Load connection settings from server.json, falling back to defaults.
pub fn load_server_prefs() -> ServerPrefs {
    load_json(&server_prefs_path()).unwrap_or_default()
}

/// Persist connection settings to server.json (GUI, CLI and TUI share this file).
pub fn save_server_prefs(prefs: &ServerPrefs) -> Result<(), String> {
    let _guard = SERVER_PREFS_WRITE.lock().unwrap_or_else(|p| p.into_inner());
    let current = load_server_prefs();
    let mut next = prefs.clone();
    next.downlink_source_id = current.downlink_source_id.clone();
    if next.output_device != current.output_device
        && next.output_endpoint_id == current.output_endpoint_id
    {
        next.output_endpoint_id = None;
    }
    normalize_selection(&current, &mut next)?;
    save_json(&server_prefs_path(), &next)
}

fn normalize_selection(current: &ServerPrefs, next: &mut ServerPrefs) -> Result<(), String> {
    #[cfg(windows)]
    {
        if next.output_device != current.output_device
            || next.output_endpoint_id != current.output_endpoint_id
        {
            let endpoints = micyou_audio::endpoint::list_output_endpoints()?;
            normalize_selection_from_endpoints(next, &endpoints)?;
        }
    }
    #[cfg(not(windows))]
    let _ = (current, next);
    Ok(())
}

#[cfg(any(windows, test))]
fn normalize_selection_from_endpoints(
    next: &mut ServerPrefs,
    endpoints: &[micyou_audio::endpoint::OutputEndpoint],
) -> Result<(), String> {
    let selected = crate::audio_routing::resolve_endpoint(
        endpoints,
        next.output_endpoint_id.as_deref(),
        &next.output_device,
    )?;
    // Auto remains unbound until an alias really opens. A pre-bound first
    // alias would suppress the ordinary-line fallback on an in-use alias.
    if next.output_endpoint_id.is_none()
        && crate::server::output::normalize_output_device(&next.output_device).is_none()
    {
        return Ok(());
    }
    next.output_endpoint_id = Some(selected.id.clone());
    next.output_device = selected.name.clone();
    Ok(())
}

/// Sparse updates preserve fields owned by another frontend under one lock.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerPrefsPatch {
    pub port: Option<u16>,
    pub web_port: Option<u16>,
    pub mode: Option<String>,
    pub bind_address: Option<String>,
    pub auto_bind: Option<bool>,
    pub output_device: Option<String>,
    pub output_endpoint_id: Option<String>,
    pub mute_sync: Option<bool>,
}

pub fn patch_server_prefs(patch: ServerPrefsPatch) -> Result<(), String> {
    let _guard = SERVER_PREFS_WRITE.lock().unwrap_or_else(|p| p.into_inner());
    let current = load_server_prefs();
    let mut next = merge_patch(&current, patch);
    normalize_selection(&current, &mut next)?;
    save_json(&server_prefs_path(), &next)
}

fn merge_patch(current: &ServerPrefs, patch: ServerPrefsPatch) -> ServerPrefs {
    let mut next = current.clone();
    if patch.output_device.is_some() && patch.output_endpoint_id.is_none() {
        next.output_endpoint_id = None;
    }
    if let Some(v) = patch.port {
        next.port = v;
    }
    if let Some(v) = patch.web_port {
        next.web_port = v;
    }
    if let Some(v) = patch.mode {
        next.mode = v;
    }
    if let Some(v) = patch.bind_address {
        next.bind_address = v;
    }
    if let Some(v) = patch.auto_bind {
        next.auto_bind = v;
    }
    if let Some(v) = patch.output_device {
        next.output_device = v;
    }
    if let Some(v) = patch.output_endpoint_id {
        next.output_endpoint_id = Some(v);
    }
    if let Some(v) = patch.mute_sync {
        next.mute_sync = v;
    }
    next
}

static SERVER_PREFS_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 原子合并各界面拥有的字段，避免来源与连接设置互相覆盖。
pub fn update_server_prefs(update: impl FnOnce(&mut ServerPrefs)) -> Result<(), String> {
    let _guard = SERVER_PREFS_WRITE.lock().unwrap_or_else(|p| p.into_inner());
    let mut prefs = load_server_prefs();
    update(&mut prefs);
    save_json(&server_prefs_path(), &prefs)
}

pub fn save_connection_prefs(prefs: &ServerPrefs) -> Result<(), String> {
    save_server_prefs(prefs)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saving_auto_validates_line_without_binding_unopened_alias() {
        use micyou_audio::endpoint::{Component, OutputEndpoint};
        let endpoints = vec![OutputEndpoint {
            id: "first-in-use".into(),
            name: "Cable".into(),
            component: Component::Cable,
            backing_id: Some("ordinary".into()),
        }];
        let mut prefs = ServerPrefs {
            output_device: "auto".into(),
            ..Default::default()
        };
        normalize_selection_from_endpoints(&mut prefs, &endpoints).unwrap();
        assert!(prefs.output_endpoint_id.is_none());
        assert_eq!(prefs.output_device, "auto");
        prefs.output_device = "Cable".into();
        normalize_selection_from_endpoints(&mut prefs, &endpoints).unwrap();
        assert_eq!(prefs.output_endpoint_id.as_deref(), Some("first-in-use"));
    }
    #[test]
    fn sparse_updates_preserve_route_and_other_frontend_source() {
        let current = ServerPrefs {
            output_device: "Cable".into(),
            output_endpoint_id: Some("stable-id".into()),
            downlink_source_id: Some("hifi".into()),
            ..Default::default()
        };
        let patch: ServerPrefsPatch = serde_json::from_str(r#"{"port":18554}"#).unwrap();
        let next = merge_patch(&current, patch);
        assert_eq!(next.output_endpoint_id, current.output_endpoint_id);
        assert_eq!(next.output_device, current.output_device);
        assert_eq!(next.downlink_source_id, current.downlink_source_id);
        assert_eq!(next.port, 18554);
        let changed = merge_patch(
            &current,
            ServerPrefsPatch {
                output_device: Some("New cable".into()),
                ..Default::default()
            },
        );
        assert!(changed.output_endpoint_id.is_none());
        let renamed = merge_patch(
            &current,
            ServerPrefsPatch {
                output_device: Some("Renamed".into()),
                output_endpoint_id: Some("stable-id".into()),
                ..Default::default()
            },
        );
        assert_eq!(renamed.output_endpoint_id, current.output_endpoint_id);
    }
    #[test]
    fn legacy_config_defaults_source_without_persisting_enable_state() {
        let prefs: ServerPrefs =
            serde_json::from_str(r#"{"port":8554,"outputDevice":"Cable"}"#).unwrap();
        assert_eq!(prefs.downlink_source_id, None);
        assert_eq!(prefs.output_device, "Cable");
        let value = serde_json::to_value(prefs).unwrap();
        assert!(value.get("downlinkSourceId").unwrap().is_null());
        assert!(value.get("requested").is_none());
        assert!(value.get("active").is_none());
    }
}
