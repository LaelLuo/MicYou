use super::lease::{Backend, Owner};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use windows::core::{implement, IUnknown_Vtbl, Interface, GUID, HRESULT, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, HANDLE, PROPERTYKEY, STILL_ACTIVE,
    WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};
use windows::Win32::System::Threading::*;

// IPolicyConfigWin7 的槽位来自已核查接口定义；只暴露 SetDefaultEndpoint。
windows::core::define_interface!(
    PolicyConfig,
    PolicyConfigVtbl,
    0xf8679f50_850a_41cf_9c72_430f290290c8
);
#[repr(C)]
pub struct PolicyConfigVtbl {
    base: IUnknown_Vtbl,
    preceding_methods: [usize; 10],
    set_default_endpoint:
        unsafe extern "system" fn(*mut std::ffi::c_void, PCWSTR, ERole) -> HRESULT,
    set_endpoint_visibility: usize,
}

#[derive(Default)]
pub(super) struct Notifications {
    pub(super) expected: [VecDeque<String>; 3],
    pub(super) external: [bool; 3],
    pub(super) invalidated: Vec<String>,
}

impl Notifications {
    fn invalidate(&mut self, endpoint: String) {
        if !self
            .invalidated
            .iter()
            .any(|id| id.eq_ignore_ascii_case(&endpoint))
        {
            if self.invalidated.len() < 256 {
                self.invalidated.push(endpoint);
            } else {
                self.invalidated[0].clear();
            }
        }
    }
    pub(super) fn failed_write(&mut self, role: usize, endpoint: &str, installed: bool) {
        if !installed
            && self.expected[role]
                .back()
                .is_some_and(|id| id.eq_ignore_ascii_case(endpoint))
        {
            self.expected[role].pop_back();
        }
    }
    pub(super) fn observed(&mut self, role: usize, endpoint: String) {
        if self.expected[role]
            .front()
            .is_some_and(|id| id.eq_ignore_ascii_case(&endpoint))
        {
            self.expected[role].pop_front();
            log::info!("[Routing] notification role={role} endpoint={endpoint} classification=own");
        } else {
            log::warn!("[Routing] notification role={role} endpoint={endpoint} classification=external expected={:?}", self.expected[role]);
            self.external[role] = true;
            self.expected[role].clear();
        }
    }
}

#[implement(IMMNotificationClient)]
struct NotificationClient {
    notifications: Arc<Mutex<Notifications>>,
}

impl IMMNotificationClient_Impl for NotificationClient_Impl {
    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        id: &PCWSTR,
    ) -> windows::core::Result<()> {
        if flow == eRender && (0..3).contains(&role.0) {
            let endpoint = if id.is_null() {
                String::new()
            } else {
                unsafe { id.to_string()? }
            };
            self.notifications
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .observed(role.0 as usize, endpoint);
        }
        Ok(())
    }
    fn OnDeviceStateChanged(&self, id: &PCWSTR, state: DEVICE_STATE) -> windows::core::Result<()> {
        if state != DEVICE_STATE_ACTIVE && !id.is_null() {
            self.notifications
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .invalidate(unsafe { id.to_string()? });
        }
        Ok(())
    }
    fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDeviceRemoved(&self, id: &PCWSTR) -> windows::core::Result<()> {
        if !id.is_null() {
            self.notifications
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .invalidate(unsafe { id.to_string()? });
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

pub(super) struct NativeBackend {
    enumerator: IMMDeviceEnumerator,
    policy: PolicyConfig,
    client: IMMNotificationClient,
    notifications: Arc<Mutex<Notifications>>,
    mutex: HANDLE,
    locked: bool,
    owner: Owner,
    // 最后释放 COM apartment，确保其上所有接口先析构。
    _apartment: crate::stereo_loopback::Apartment,
}

fn process_creation(handle: HANDLE) -> Result<u64, String> {
    let mut created = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe { GetProcessTimes(handle, &mut created, &mut exit, &mut kernel, &mut user) }
        .map_err(|error| error.to_string())?;
    Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

fn user_mutex_name() -> Result<Vec<u16>, String> {
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .map_err(|error| error.to_string())?;
    let result = (|| {
        let mut needed = 0;
        let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut needed) };
        if needed == 0 || needed > 65536 {
            return Err("unable to resolve routing mutex user".into());
        }
        // usize 对齐满足 TOKEN_USER；不能把 Vec<u8> 当作对齐结构读取。
        let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
        unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                Some(buffer.as_mut_ptr().cast()),
                needed,
                &mut needed,
            )
        }
        .map_err(|error| error.to_string())?;
        let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        let mut text = windows::core::PWSTR::null();
        unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) }
            .map_err(|error| error.to_string())?;
        let sid = unsafe { text.to_string() }.map_err(|error| error.to_string());
        unsafe {
            windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(
                text.0.cast(),
            )))
        };
        let sid = sid?;
        Ok(format!("Global\\com.laelluo.micyou.duplex.routing.{sid}\0")
            .encode_utf16()
            .collect())
    })();
    unsafe { CloseHandle(token) }.ok();
    result
}

impl NativeBackend {
    pub fn new() -> Result<Self, String> {
        let apartment =
            crate::stereo_loopback::Apartment::enter().map_err(|error| error.to_string())?;
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                .map_err(|error| error.to_string())?;
        let policy: PolicyConfig = unsafe {
            CoCreateInstance(
                &GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9),
                None,
                CLSCTX_ALL,
            )
        }
        .map_err(|error| error.to_string())?;
        let notifications = Arc::new(Mutex::new(Notifications::default()));
        let client: IMMNotificationClient = NotificationClient {
            notifications: notifications.clone(),
        }
        .into();
        let pid = std::process::id();
        let created = process_creation(unsafe { GetCurrentProcess() })?;
        let instance = format!(
            "{pid}-{created}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let name = user_mutex_name()?;
        let mutex = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }
            .map_err(|error| error.to_string())?;
        if let Err(error) = unsafe { enumerator.RegisterEndpointNotificationCallback(&client) } {
            unsafe { CloseHandle(mutex) }.ok();
            return Err(error.to_string());
        }
        Ok(Self {
            enumerator,
            policy,
            client,
            notifications,
            mutex,
            locked: false,
            owner: Owner {
                pid,
                created,
                instance,
            },
            _apartment: apartment,
        })
    }
}

impl Backend for NativeBackend {
    fn try_lock(&mut self) -> Result<(), String> {
        match unsafe { WaitForSingleObject(self.mutex, 0) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => {
                self.locked = true;
                Ok(())
            }
            WAIT_TIMEOUT => {
                Err("route_busy: computer playback routing is owned by another instance".into())
            }
            _ => Err(format!(
                "routing mutex wait failed: {}",
                std::io::Error::last_os_error()
            )),
        }
    }
    fn unlock(&mut self) {
        if self.locked {
            unsafe { ReleaseMutex(self.mutex) }.ok();
            self.locked = false;
        }
    }
    fn owner(&self) -> Owner {
        self.owner.clone()
    }
    fn owner_alive(&self, owner: &Owner) -> Result<bool, String> {
        let process =
            match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, owner.pid) } {
                Ok(process) => process,
                Err(error) if error.code() == HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) => {
                    return Ok(false)
                }
                Err(error) => return Err(format!("cannot verify routing owner: {error}")),
            };
        let result = (|| {
            let creation = process_creation(process)?;
            let mut exit = 0;
            unsafe { GetExitCodeProcess(process, &mut exit) }.map_err(|error| error.to_string())?;
            Ok(creation == owner.created && exit == STILL_ACTIVE.0 as u32)
        })();
        unsafe { CloseHandle(process) }.ok();
        result
    }
    fn defaults(&self) -> Result<[String; 3], String> {
        let mut defaults = [String::new(), String::new(), String::new()];
        for (role, value) in defaults.iter_mut().enumerate() {
            *value = self.default_render(role)?;
        }
        Ok(defaults)
    }
    fn default_render(&self, role: usize) -> Result<String, String> {
        let device = unsafe {
            self.enumerator
                .GetDefaultAudioEndpoint(eRender, ERole(role as i32))
        }
        .map_err(|error| error.to_string())?;
        let id = unsafe { device.GetId() }.map_err(|error| error.to_string())?;
        let text = unsafe { id.to_string() }.map_err(|error| error.to_string());
        unsafe { CoTaskMemFree(Some(id.0.cast())) };
        text
    }
    fn set_render(&mut self, role: usize, endpoint: &str) -> Result<(), String> {
        if role >= 3 || endpoint.is_empty() || endpoint.contains('\0') {
            return Err("invalid render role or endpoint".into());
        }
        let wide: Vec<u16> = endpoint.encode_utf16().chain(Some(0)).collect();
        let device = unsafe { self.enumerator.GetDevice(PCWSTR(wide.as_ptr())) }
            .map_err(|error| error.to_string())?;
        let identity: IMMEndpoint = device.cast().map_err(|error| error.to_string())?;
        if unsafe { identity.GetDataFlow() }.map_err(|error| error.to_string())? != eRender {
            return Err("capture endpoints cannot be written by playback routing".into());
        }
        let affected = self.write_roles(role);
        let mut before = [None, None, None];
        for (index, changed) in affected.into_iter().enumerate() {
            if changed {
                before[index] = Some(self.default_render(index)?);
            }
        }
        {
            let mut notifications = self.notifications.lock().unwrap_or_else(|p| p.into_inner());
            for (index, changed) in affected.into_iter().enumerate() {
                if changed && !before[index].as_deref().is_some_and(|id| id.eq_ignore_ascii_case(endpoint)) {
                    notifications.expected[index].push_back(endpoint.into());
                }
            }
        }
        log::info!("[Routing] setter role={role} affected={affected:?} before={before:?} target={endpoint}");
        let result = unsafe {
            (self.policy.vtable().set_default_endpoint)(
                self.policy.as_raw(),
                PCWSTR(wide.as_ptr()),
                ERole(role as i32),
            )
        }
        .ok();
        log::info!(
            "[Routing] setter role={role} target={endpoint} result={result:?} after={:?}",
            self.defaults()
        );
        if result.is_err() {
            // 失败 HRESULT 不等于没有写入；回读已安装目标时仍须消费晚到的自身通知。
            for (index, changed) in affected.into_iter().enumerate() {
                if changed {
                    if let Ok(actual) = self.default_render(index) {
                        self.notifications
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .failed_write(index, endpoint, actual.eq_ignore_ascii_case(endpoint));
                    }
                }
            }
        }
        result.map_err(|error| error.to_string())
    }
    fn write_roles(&self, role: usize) -> [bool; 3] {
        match role {
            0 | 1 => [true, true, false],
            2 => [false, false, true],
            _ => [false; 3],
        }
    }
    fn external_changes(&mut self) -> [bool; 3] {
        let mut state = self.notifications.lock().unwrap_or_else(|p| p.into_inner());
        std::mem::take(&mut state.external)
    }
    fn invalidated_endpoints(&mut self) -> Vec<String> {
        std::mem::take(
            &mut self
                .notifications
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .invalidated,
        )
    }
    fn reset_notifications(&mut self) {
        // 上次恢复的通知可能晚到；保留已排队的自身写入，避免把它误认为新 lease 的外部操作。
        self.notifications
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .external = [false; 3];
        self.notifications
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .invalidated
            .clear();
    }
}

impl Drop for NativeBackend {
    fn drop(&mut self) {
        unsafe {
            self.enumerator
                .UnregisterEndpointNotificationCallback(&self.client)
        }
        .ok();
        self.unlock();
        unsafe { CloseHandle(self.mutex) }.ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn own_notifications_are_consumed_but_external_out_and_back_is_latched() {
        let mut events = Notifications::default();
        events.expected[0].push_back("hifi".into());
        events.observed(0, "HIFI".into());
        assert_eq!(events.external, [false; 3]);
        events.observed(0, "speakers".into());
        events.observed(0, "hifi".into());
        assert_eq!(events.external, [true, false, false]);
    }
    #[test]
    fn external_notification_interleaved_with_setter_does_not_get_swallowed() {
        let mut events = Notifications::default();
        events.expected[1].push_back("hifi".into());
        events.observed(1, "headphones".into());
        events.observed(1, "hifi".into());
        assert!(events.external[1]);
        assert!(events.expected[1].is_empty());
    }
    #[test]
    fn failed_but_installed_setter_keeps_late_self_notification_owned() {
        let mut events = Notifications::default();
        events.expected[0].push_back("hifi".into());
        events.failed_write(0, "hifi", true);
        events.expected[0].push_back("speakers".into());
        events.observed(0, "hifi".into());
        events.observed(0, "speakers".into());
        assert_eq!(events.external, [false; 3]);
        assert!(events.expected[0].is_empty());
    }
    #[test]
    fn failed_uninstalled_setter_does_not_swallow_future_user_selection() {
        let mut events = Notifications::default();
        events.expected[0].push_back("hifi".into());
        events.failed_write(0, "hifi", false);
        events.observed(0, "hifi".into());
        assert!(events.external[0]);
    }
    #[test]
    fn device_removal_is_latched_separately_from_default_ownership_changes() {
        let mut events = Notifications::default();
        events.invalidate("speakers".into());
        events.invalidate("SPEAKERS".into());
        assert_eq!(events.invalidated, ["speakers"]);
        assert_eq!(events.external, [false; 3]);
    }
}
