//! Shared selection policy for GUI, CLI and TUI. Loading preferences has no
//! device side effects; migration is performed only at an explicit operation.
use micyou_audio::endpoint::{Component, OutputEndpoint};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AudioRoutingStatus {
    pub output_endpoint_id: Option<String>,
    pub monitor_endpoint_id: Option<String>,
    pub aec_reference_id: Option<String>,
    pub aec_desired_reference_id: Option<String>,
    pub aec_state: AecState,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AecState {
    #[default]
    Disabled,
    Preparing,
    Running,
    MonitorConflict,
    IdentityUnavailable,
    CaptureFailed,
}

pub fn resolve_endpoint<'a>(
    endpoints: &'a [OutputEndpoint],
    id: Option<&str>,
    legacy: &str,
) -> Result<&'a OutputEndpoint, String> {
    if let Some(id) = id.filter(|id| !id.is_empty()) {
        return endpoints
            .iter()
            .find(|e| e.id.eq_ignore_ascii_case(id))
            .ok_or_else(|| "output_endpoint_unavailable".into());
    }
    let auto = legacy.trim().is_empty()
        || ["auto", "default"]
            .iter()
            .any(|s| legacy.eq_ignore_ascii_case(s));
    let mut candidates: Vec<_> = endpoints
        .iter()
        .filter(|e| {
            if auto {
                e.component == Component::Cable
            } else {
                e.name == legacy
            }
        })
        .collect();
    if candidates.is_empty() {
        return Err("output_endpoint_unavailable".into());
    }
    if auto {
        // A driver's 2ch and 16ch render aliases belong to one ordinary line.
        // Endpoint ordering makes the choice deterministic across enumeration.
        candidates.sort_by(|a, b| a.id.cmp(&b.id));
        let backing = candidates[0]
            .backing_id
            .as_ref()
            .ok_or("identity_unavailable")?;
        if candidates.iter().any(|e| {
            e.backing_id
                .as_ref()
                .is_none_or(|id| !id.eq_ignore_ascii_case(backing))
        }) {
            return Err("output_endpoint_ambiguous".into());
        }
    } else if candidates.len() != 1 {
        return Err("output_endpoint_ambiguous".into());
    }
    Ok(candidates[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    fn e(id: &str, name: &str, component: Component, backing: &str) -> OutputEndpoint {
        OutputEndpoint {
            id: id.into(),
            name: name.into(),
            component,
            backing_id: Some(backing.into()),
        }
    }
    #[test]
    fn auto_groups_aliases_and_excludes_hifi() {
        let endpoints = vec![
            e("b", "16ch", Component::Cable, "ordinary"),
            e("a", "2ch", Component::Cable, "ordinary"),
            e("c", "HIFI", Component::Hifi, "hifi"),
        ];
        assert_eq!(resolve_endpoint(&endpoints, None, "auto").unwrap().id, "a");
        let mut duplicate = endpoints.clone();
        duplicate.push(e("d", "other", Component::Cable, "second"));
        assert_eq!(
            resolve_endpoint(&duplicate, None, "").unwrap_err(),
            "output_endpoint_ambiguous"
        );
    }
    #[test]
    fn stable_identity_never_falls_back_to_name() {
        let endpoints = vec![e("a", "renamed", Component::Physical, "physical")];
        assert_eq!(
            resolve_endpoint(&endpoints, Some("a"), "old").unwrap().id,
            "a"
        );
        assert_eq!(
            resolve_endpoint(&endpoints, Some("missing"), "renamed").unwrap_err(),
            "output_endpoint_unavailable"
        );
    }
    #[test]
    fn explicit_physical_is_allowed_but_duplicate_names_are_not_migrated() {
        let endpoints = vec![
            e("a", "Speaker", Component::Physical, "card1"),
            e("b", "Speaker", Component::Physical, "card2"),
        ];
        assert_eq!(
            resolve_endpoint(&endpoints, None, "Speaker").unwrap_err(),
            "output_endpoint_ambiguous"
        );
        assert_eq!(
            resolve_endpoint(&endpoints, Some("A"), "old name")
                .unwrap()
                .id,
            "a"
        );
        assert_eq!(
            resolve_endpoint(&endpoints, None, "auto").unwrap_err(),
            "output_endpoint_unavailable"
        );
    }
}
