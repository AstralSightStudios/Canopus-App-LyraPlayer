//! The CanopusConnect side of the plugin: the host calls (`send`, `listen`) and the
//! device list. The Lyra Player module on the watch is addressed by its module id.

use std::{collections::HashSet, sync::Mutex};

use lyra_protocol::{FRAME_MAX, MODULE_ID};

use crate::{
    astrobox::psys_host::device,
    astrobox::psys_host_v4::canopusconnect::{self as host, Payload},
    state::{self, DeviceInfo},
};

static LISTENING: Mutex<Option<HashSet<String>>> = Mutex::new(None);

pub fn refresh_devices() {
    let devices = wit_bindgen::block_on(device::get_connected_device_list().into_future())
        .into_iter()
        .map(|item| DeviceInfo {
            addr: item.addr,
            name: item.name,
        })
        .collect::<Vec<_>>();
    state::with_state(|state| {
        state.devices = devices;
        if !state
            .devices
            .iter()
            .any(|item| item.addr == state.selected_addr)
        {
            state.selected_addr = state
                .devices
                .first()
                .map(|item| item.addr.clone())
                .unwrap_or_default();
            state.device_library.clear();
            state.library_revision.clear();
            state.library_total = 0;
        }
    });
    // A device that went away forgot the registration.
    let known: HashSet<String> = state::snapshot().devices.into_iter().map(|d| d.addr).collect();
    if let Some(listening) = LISTENING.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        listening.retain(|addr| known.contains(addr));
    }
}

/// Asks the device to push the module's messages to this phone; once per device.
pub async fn ensure_listening(addr: &str) -> Result<(), String> {
    let already = LISTENING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|set| set.contains(addr));
    if already {
        return Ok(());
    }
    host::listen(addr, MODULE_ID).into_future().await?;
    LISTENING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashSet::new)
        .insert(addr.to_string());
    Ok(())
}

/// One frame to the module; resolves when the module has taken it.
pub async fn send(addr: &str, frame: &[u8]) -> Result<(), String> {
    if frame.len() > FRAME_MAX {
        return Err("frame is longer than 480 bytes".to_string());
    }
    host::send(addr, MODULE_ID, &Payload::Binary(frame.to_vec()))
        .into_future()
        .await
}

/// The message of a `canopusconnect.on-message` call, if it is a binary frame from
/// the Lyra Player module.
pub fn binary_frame(module_id: &str, payload: Payload) -> Option<Vec<u8>> {
    match payload {
        Payload::Binary(bytes) if module_id == MODULE_ID => Some(bytes),
        _ => None,
    }
}
