// Files dropped on the capsule (it shows up while a file is being dragged, see island.rs).
// The capsule window is an OLE drop target; a drop hands the file paths to the uploader.

use std::path::PathBuf;

use windows::core::{implement, Result};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Ole::*;
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::upload;

/// wparam 1: a file drag is over the capsule, 0: it left again. 2: dropped.
pub const WM_APP_DROPOVER: u32 = WM_APP + 17;

#[implement(IDropTarget)]
struct DropZone {
    hwnd: isize,
}

fn file_format() -> FORMATETC {
    FORMATETC { cfFormat: CF_HDROP.0, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 }
}

impl DropZone {
    fn tell(&self, state: usize) {
        unsafe {
            let _ = PostMessageW(HWND(self.hwnd as _), WM_APP_DROPOVER, WPARAM(state), LPARAM(0));
        }
    }
}

unsafe fn accept(data: Option<&IDataObject>, effect: *mut DROPEFFECT) -> bool {
    let files = data.is_some_and(|d| d.QueryGetData(&file_format()) == S_OK);
    if !effect.is_null() {
        *effect = if files { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
    }
    files
}

impl IDropTarget_Impl for DropZone_Impl {
    fn DragEnter(&self, data: Option<&IDataObject>, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        if unsafe { accept(data, effect) } {
            self.tell(1);
        }
        Ok(())
    }

    fn DragOver(&self, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        if !effect.is_null() {
            unsafe { *effect = DROPEFFECT_COPY };
        }
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        self.tell(0);
        Ok(())
    }

    fn Drop(&self, data: Option<&IDataObject>, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        unsafe {
            if !accept(data, effect) {
                return Ok(());
            }
            let mut medium = data.unwrap().GetData(&file_format())?;
            let drop = HDROP(medium.u.hGlobal.0);
            let mut files: Vec<PathBuf> = Vec::new();
            for i in 0..DragQueryFileW(drop, u32::MAX, None) {
                let mut buf = [0u16; 520];
                let len = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
                files.push(PathBuf::from(String::from_utf16_lossy(&buf[..len])));
            }
            ReleaseStgMedium(&mut medium);
            files.retain(|p| p.is_file());
            self.tell(2);
            upload::enqueue(files, self.hwnd, crate::hook::WM_APP_UPLOAD);
        }
        Ok(())
    }
}

/// Makes `hwnd` accept dragged files. OLE must be initialised on this thread.
pub unsafe fn register(hwnd: HWND) {
    let target: IDropTarget = DropZone { hwnd: hwnd.0 as isize }.into();
    let _ = RegisterDragDrop(hwnd, &target);
}
