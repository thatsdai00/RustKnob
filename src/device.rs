// Default output device: change notifications and the friendly device name.

use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};

use windows::core::{implement, Result, PCWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::*;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

static TARGET: AtomicIsize = AtomicIsize::new(0);
static MSG: AtomicIsize = AtomicIsize::new(0);
/// Bumped whenever a default playback device (any role) changes: the visualizer's capture follows it.
pub static RENDER_CHANGES: AtomicU32 = AtomicU32::new(0);

#[implement(IMMNotificationClient)]
struct Watcher;

impl IMMNotificationClient_Impl for Watcher_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> Result<()> {
        Ok(())
    }
    fn OnDeviceAdded(&self, _: &PCWSTR) -> Result<()> {
        Ok(())
    }
    fn OnDeviceRemoved(&self, _: &PCWSTR) -> Result<()> {
        Ok(())
    }
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _: &PCWSTR) -> Result<()> {
        if flow == eRender {
            RENDER_CHANGES.fetch_add(1, Ordering::Relaxed);
        }
        if flow == eRender && role == eMultimedia {
            unsafe {
                let _ = PostMessageW(
                    HWND(TARGET.load(Ordering::Relaxed) as _),
                    MSG.load(Ordering::Relaxed) as u32,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> Result<()> {
        Ok(())
    }
}

/// Keeps the registration alive for as long as it is held.
pub struct Subscription {
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        unsafe {
            let _ = self.enumerator.UnregisterEndpointNotificationCallback(&self.client);
        }
    }
}

/// Posts `msg` to `hwnd` whenever the default playback device changes.
pub unsafe fn watch(hwnd: isize, msg: u32) -> Option<Subscription> {
    TARGET.store(hwnd, Ordering::Relaxed);
    MSG.store(msg as isize, Ordering::Relaxed);
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
    let client: IMMNotificationClient = Watcher.into();
    enumerator.RegisterEndpointNotificationCallback(&client).ok()?;
    Some(Subscription { enumerator, client })
}

/// Friendly name of the current default playback device, e.g. "Speakers (Realtek Audio)".
pub unsafe fn default_name() -> Option<String> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
    let device = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia).ok()?;
    let store = device.OpenPropertyStore(STGM_READ).ok()?;
    let value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
    let name = PropVariantToStringAlloc(&value).ok()?;
    let text = name.to_string().ok();
    CoTaskMemFree(Some(name.0 as *const _));
    text
}
