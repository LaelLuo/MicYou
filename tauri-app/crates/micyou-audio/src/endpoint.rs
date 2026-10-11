//! Endpoint identity is independent of its localized, mutable display name.
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Component {
    Cable,
    Hifi,
    Physical,
    UnknownVirtual,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputEndpoint {
    pub id: String,
    pub name: String,
    pub component: Component,
    pub backing_id: Option<String>,
}

pub fn same_line(a: &OutputEndpoint, b: &OutputEndpoint) -> bool {
    a.id.eq_ignore_ascii_case(&b.id)
        || (matches!(
            (a.component, b.component),
            (Component::Cable, Component::Cable) | (Component::Hifi, Component::Hifi)
        ) && a
            .backing_id
            .as_ref()
            .zip(b.backing_id.as_ref())
            .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b)))
}

pub fn list_output_endpoints() -> Result<Vec<OutputEndpoint>, String> {
    crate::stereo_loopback::list_render_endpoints()?
        .into_iter()
        .map(|e| {
            let (component, backing_id) = identity(&e.id)?;
            Ok(OutputEndpoint {
                id: e.id,
                name: e.name,
                component,
                backing_id,
            })
        })
        .collect()
}

/// Resolve only one native devnode; callers need not enumerate all devices.
pub fn resolve_endpoint_identity(id: &str) -> Result<OutputEndpoint, String> {
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    type Cache = std::collections::HashMap<String, (Instant, OutputEndpoint)>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(Cache::new()));
    let key = id.to_ascii_uppercase();
    if let Some((time, endpoint)) = cache.lock().unwrap_or_else(|p| p.into_inner()).get(&key) {
        if time.elapsed() < Duration::from_secs(1) {
            return Ok(endpoint.clone());
        }
    }
    let (component, backing_id) = identity(id)?;
    let endpoint = OutputEndpoint {
        id: id.into(),
        name: String::new(),
        component,
        backing_id,
    };
    let mut cache = cache.lock().unwrap_or_else(|p| p.into_inner());
    if cache.len() >= 256 {
        cache.clear();
    }
    cache.insert(key, (Instant::now(), endpoint.clone()));
    Ok(endpoint)
}

#[cfg(windows)]
pub fn default_render_endpoint_id() -> Result<String, String> {
    use wasapi::{DeviceEnumerator, Direction};
    let _apartment = crate::stereo_loopback::Apartment::query().map_err(|e| e.to_string())?;
    let result = DeviceEnumerator::new()
        .and_then(|e| e.get_default_device(&Direction::Render))
        .and_then(|e| e.get_id());
    result.map_err(|e| e.to_string())
}

#[cfg(not(windows))]
pub fn default_render_endpoint_id() -> Result<String, String> {
    Err("unsupported_platform".into())
}

#[cfg(not(windows))]
fn identity(_: &str) -> Result<(Component, Option<String>), String> {
    Ok((Component::Physical, None))
}

#[cfg(windows)]
fn identity(endpoint: &str) -> Result<(Component, Option<String>), String> {
    // Configuration Manager exposes the real endpoint devnode and its parents;
    // friendly names and shared vendor ContainerIds are deliberately not used.
    use windows::core::GUID;
    #[repr(C)]
    struct PropertyKey {
        fmtid: GUID,
        pid: u32,
    }
    #[link(name = "cfgmgr32")]
    unsafe extern "system" {
        fn CM_Locate_DevNodeW(node: *mut u32, id: *const u16, flags: u32) -> u32;
        fn CM_Get_Parent(parent: *mut u32, node: u32, flags: u32) -> u32;
        fn CM_Get_Device_IDW(node: u32, buffer: *mut u16, len: u32, flags: u32) -> u32;
        fn CM_Get_DevNode_PropertyW(
            node: u32,
            key: *const PropertyKey,
            kind: *mut u32,
            buffer: *mut u8,
            size: *mut u32,
            flags: u32,
        ) -> u32;
    }
    fn property(node: u32, pid: u32) -> Option<String> {
        let key = PropertyKey {
            fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
            pid,
        };
        let mut buffer = [0u16; 2048];
        let mut bytes = (buffer.len() * 2) as u32;
        let mut kind = 0;
        let result = unsafe {
            CM_Get_DevNode_PropertyW(
                node,
                &key,
                &mut kind,
                buffer.as_mut_ptr().cast(),
                &mut bytes,
                0,
            )
        };
        if result != 0
            || !bytes.is_multiple_of(2)
            || bytes as usize > buffer.len() * 2
            || kind != if pid == 3 { 0x2012 } else { 0x12 }
        {
            return None;
        }
        Some(
            String::from_utf16_lossy(&buffer[..(bytes as usize / 2).min(buffer.len())])
                .trim_end_matches('\0')
                .to_string(),
        )
    }
    let id: Vec<u16> = format!("SWD\\MMDEVAPI\\{endpoint}\0")
        .encode_utf16()
        .collect();
    let mut node = 0;
    if unsafe { CM_Locate_DevNodeW(&mut node, id.as_ptr(), 0) } != 0 {
        return Ok((Component::UnknownVirtual, None));
    }
    for _ in 0..8 {
        let mut buffer = [0u16; 1024];
        if unsafe { CM_Get_Device_IDW(node, buffer.as_mut_ptr(), buffer.len() as u32, 0) } != 0 {
            break;
        }
        let len = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        let backing = String::from_utf16_lossy(&buffer[..len]);
        let hardware = property(node, 3).unwrap_or_default().to_ascii_uppercase();
        let service = property(node, 6).unwrap_or_default();
        let component = if hardware.split('\0').any(|h| h == "VBAUDIOVACWDM")
            && service.eq_ignore_ascii_case("VBAudioVACMME")
        {
            Some(Component::Cable)
        } else if hardware.split('\0').any(|h| h == "VBAUDIOHFVAIO")
            && service.eq_ignore_ascii_case("VBAudioHFVAIOMME")
        {
            Some(Component::Hifi)
        } else if backing.starts_with("ROOT\\MEDIA\\") {
            Some(Component::UnknownVirtual)
        } else if ["USB\\", "PCI\\", "BTHENUM\\", "BTHHFENUM\\", "HDAUDIO\\"]
            .iter()
            .any(|p| backing.starts_with(p))
        {
            Some(Component::Physical)
        } else {
            None
        };
        if let Some(component) = component {
            return Ok((component, Some(backing)));
        }
        let mut parent = 0;
        if unsafe { CM_Get_Parent(&mut parent, node, 0) } != 0 {
            break;
        }
        node = parent;
    }
    Ok((Component::UnknownVirtual, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn endpoint(id: &str, backing: Option<&str>) -> OutputEndpoint {
        OutputEndpoint {
            id: id.into(),
            name: id.into(),
            component: Component::Cable,
            backing_id: backing.map(str::to_string),
        }
    }
    #[test]
    fn aliases_share_backing_but_unknowns_do_not() {
        assert!(same_line(
            &endpoint("2ch", Some("ROOT\\MEDIA\\1")),
            &endpoint("16ch", Some("ROOT\\MEDIA\\1"))
        ));
        assert!(!same_line(&endpoint("a", None), &endpoint("b", None)));
        assert!(same_line(&endpoint("a", None), &endpoint("a", None)));
        let mut physical_a = endpoint("a", Some("card"));
        physical_a.component = Component::Physical;
        let mut physical_b = endpoint("b", Some("card"));
        physical_b.component = Component::Physical;
        assert!(!same_line(&physical_a, &physical_b));
        physical_a.component = Component::UnknownVirtual;
        physical_b.component = Component::UnknownVirtual;
        assert!(!same_line(&physical_a, &physical_b));
        assert!(same_line(
            &endpoint("ALIAS-A", Some("root\\media\\1")),
            &endpoint("alias-b", Some("ROOT\\MEDIA\\1"))
        ));
    }
}
