//! Audio output device enumeration & one-click default playback switcher for topbar (Core Audio + PolicyConfigClient).
#![allow(non_snake_case)]
use std::ffi::c_void;
use windows::{
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Media::Audio::{
            DEVICE_STATE_ACTIVE, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, eCommunications, eConsole,
            eMultimedia, eRender,
        },
        System::Com::{CLSCTX_ALL, CoCreateInstance, STGM_READ},
    },
    core::{GUID, HRESULT, PCWSTR},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dev {
    pub id: String,
    pub name: String,
    pub default: bool,
}

fn enumerator() -> Option<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok() }
}

fn dev_id(d: &IMMDevice) -> Option<String> {
    unsafe { d.GetId().ok().and_then(|p| p.to_string().ok()) }
}

fn dev_name(d: &IMMDevice) -> String {
    unsafe {
        let name = d
            .OpenPropertyStore(STGM_READ)
            .ok()
            .and_then(|s| s.GetValue(&PKEY_Device_FriendlyName).ok())
            .map(|v| v.to_string())
            .filter(|s| !s.is_empty());
        name.unwrap_or_else(|| dev_id(d).unwrap_or_default())
    }
}

/// Lists all active playback (output) devices, with the current default flagged.
pub fn outputs() -> Vec<Dev> {
    let Some(e) = enumerator() else { return vec![] };
    let cur = unsafe { e.GetDefaultAudioEndpoint(eRender, eConsole).ok() }.and_then(|d| dev_id(&d));
    let Ok(col) = (unsafe { e.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }) else { return vec![] };
    let mut list: Vec<Dev> = (0..unsafe { col.GetCount() }.unwrap_or(0))
        .filter_map(|i| unsafe { col.Item(i).ok() })
        .filter_map(|d| {
            dev_id(&d).map(|id| Dev {
                default: Some(&id) == cur.as_ref(),
                name: dev_name(&d),
                id,
            })
        })
        .collect();
    list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    list
}

/// Active output device name from a device list, or a fallback dash.
pub fn active_name(devs: &[Dev]) -> &str {
    devs.iter()
        .find(|d| d.default)
        .or_else(|| devs.first())
        .map_or("—", |d| d.name.as_str())
}

// Same COM class Windows' Sound panel uses (PolicyConfigClient) to switch the default audio output device.
#[windows::core::interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: windows::core::IUnknown {
    fn GetMixFormat(&self, dev: PCWSTR, fmt: *mut *mut c_void) -> HRESULT;
    fn GetDeviceFormat(&self, dev: PCWSTR, default: i32, fmt: *mut *mut c_void) -> HRESULT;
    fn ResetDeviceFormat(&self, dev: PCWSTR) -> HRESULT;
    fn SetDeviceFormat(&self, dev: PCWSTR, a: *mut c_void, b: *mut c_void) -> HRESULT;
    fn GetProcessingPeriod(&self, dev: PCWSTR, default: i32, a: *mut i64, b: *mut i64) -> HRESULT;
    fn SetProcessingPeriod(&self, dev: PCWSTR, a: *mut i64) -> HRESULT;
    fn GetShareMode(&self, dev: PCWSTR, m: *mut c_void) -> HRESULT;
    fn SetShareMode(&self, dev: PCWSTR, m: *mut c_void) -> HRESULT;
    fn GetPropertyValue(&self, dev: PCWSTR, fx: i32, key: *const c_void, v: *mut c_void) -> HRESULT;
    fn SetPropertyValue(&self, dev: PCWSTR, fx: i32, key: *const c_void, v: *mut c_void) -> HRESULT;
    fn SetDefaultEndpoint(&self, dev: PCWSTR, role: i32) -> HRESULT;
    fn SetEndpointVisibility(&self, dev: PCWSTR, visible: i32) -> HRESULT;
}

const POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

/// Sets `id` as the default playback device for all roles (console, multimedia, communications).
pub fn set_default(id: &str) -> bool {
    unsafe {
        let Ok(pc) = CoCreateInstance::<_, IPolicyConfig>(&POLICY_CONFIG_CLIENT, None, CLSCTX_ALL) else { return false };
        let w: Vec<u16> = id.encode_utf16().chain([0]).collect();
        [eConsole, eMultimedia, eCommunications]
            .iter()
            .all(|r| pc.SetDefaultEndpoint(PCWSTR(w.as_ptr()), r.0).is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_name_picks_default_or_first_or_dash() {
        assert_eq!(active_name(&[]), "—");
        let devs = vec![
            Dev { id: "1".into(), name: "Speakers".into(), default: false },
            Dev { id: "2".into(), name: "Headphones".into(), default: true },
        ];
        assert_eq!(active_name(&devs), "Headphones");
        let no_def = vec![Dev { id: "1".into(), name: "Speakers".into(), default: false }];
        assert_eq!(active_name(&no_def), "Speakers");
    }
}
