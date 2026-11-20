// Plug and Play device nodes: their ids and properties, read straight from the configuration manager (no
// SetupDi device sets: one property is a single call, a few microseconds).

use windows::core::{GUID, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::Properties::*;

/// A property's type and raw bytes.
pub fn prop(node: u32, key: &DEVPROPKEY) -> Option<(DEVPROPTYPE, Vec<u8>)> {
    unsafe {
        let mut ty = DEVPROPTYPE(0);
        let mut size = 0u32;
        let _ = CM_Get_DevNode_PropertyW(node, key, &mut ty, None, &mut size, 0);
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        (CM_Get_DevNode_PropertyW(node, key, &mut ty, Some(buf.as_mut_ptr()), &mut size, 0) == CR_SUCCESS).then_some((ty, buf))
    }
}

pub fn byte(node: u32, key: &DEVPROPKEY) -> Option<u8> {
    prop(node, key).filter(|(t, b)| *t == DEVPROP_TYPE_BYTE && !b.is_empty()).map(|(_, b)| b[0])
}

pub fn boolean(node: u32, key: &DEVPROPKEY) -> Option<bool> {
    prop(node, key).filter(|(t, b)| *t == DEVPROP_TYPE_BOOLEAN && !b.is_empty()).map(|(_, b)| b[0] != 0)
}

pub fn uint(node: u32, key: &DEVPROPKEY) -> Option<u32> {
    prop(node, key).filter(|(t, b)| *t == DEVPROP_TYPE_UINT32 && b.len() >= 4).map(|(_, b)| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

pub fn guid(node: u32, key: &DEVPROPKEY) -> Option<GUID> {
    prop(node, key).filter(|(t, b)| *t == DEVPROP_TYPE_GUID && b.len() >= 16).map(|(_, b)| GUID::from_u128(u128::from_be_bytes(guid_be(&b))))
}

/// A string, or the first of a string list.
pub fn string(node: u32, key: &DEVPROPKEY) -> Option<String> {
    let (t, b) = prop(node, key).filter(|(t, _)| *t == DEVPROP_TYPE_STRING || *t == DEVPROP_TYPE_STRING_LIST)?;
    let wide: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
    let _ = t;
    Some(String::from_utf16_lossy(&wide)).filter(|s| !s.is_empty())
}

/// A GUID's 16 stored bytes (Data1..3 little-endian) in the big-endian order `GUID::from_u128` takes.
fn guid_be(b: &[u8]) -> [u8; 16] {
    let mut o = [0u8; 16];
    o[..4].copy_from_slice(&[b[3], b[2], b[1], b[0]]);
    o[4..6].copy_from_slice(&[b[5], b[4]]);
    o[6..8].copy_from_slice(&[b[7], b[6]]);
    o[8..].copy_from_slice(&b[8..16]);
    o
}

/// The instance ids of the present devices of one enumerator ("BTHENUM").
pub fn ids(enumerator: &str) -> Vec<String> {
    let filter: Vec<u16> = enumerator.encode_utf16().chain(Some(0)).collect();
    let flags = CM_GETIDLIST_FILTER_ENUMERATOR | CM_GETIDLIST_FILTER_PRESENT;
    unsafe {
        let mut len = 0u32;
        if CM_Get_Device_ID_List_SizeW(&mut len, PCWSTR(filter.as_ptr()), flags) != CR_SUCCESS || len == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        if CM_Get_Device_ID_ListW(PCWSTR(filter.as_ptr()), &mut buf, flags) != CR_SUCCESS {
            return Vec::new();
        }
        buf.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
    }
}

/// The device node of an instance id.
pub fn locate(id: &str) -> Option<u32> {
    let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
    let mut node = 0u32;
    (unsafe { CM_Locate_DevNodeW(&mut node, PCWSTR(wide.as_ptr()), CM_LOCATE_DEVNODE_NORMAL) } == CR_SUCCESS).then_some(node)
}

/// Windows marks the node as not working (a driver problem).
pub fn has_problem(node: u32) -> bool {
    let (mut status, mut problem) = (CM_DEVNODE_STATUS_FLAGS(0), CM_PROB(0));
    unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, node, 0) == CR_SUCCESS && status.0 & DN_HAS_PROBLEM.0 != 0 }
}

/// The physical device (container) a HID collection belongs to: one id for all its interfaces and transports.
pub fn container_of(path: &[u16]) -> Option<GUID> {
    unsafe {
        let mut ty = DEVPROPTYPE(0);
        let mut buf = [0u8; 512];
        let mut size = buf.len() as u32;
        if CM_Get_Device_Interface_PropertyW(PCWSTR(path.as_ptr()), &DEVPKEY_Device_InstanceId, &mut ty, Some(buf.as_mut_ptr()), &mut size, 0) != CR_SUCCESS {
            return None;
        }
        let wide: Vec<u16> = buf[..size as usize].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
        guid(locate(&String::from_utf16_lossy(&wide))?, &DEVPKEY_Device_ContainerId)
    }
}
