use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};

pub fn set_visible(hwnd: HWND, visible: bool) -> windows::core::Result<()> {
    unsafe {
        let taskbar: ITaskbarList = CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER)?;
        taskbar.HrInit()?;
        if visible {
            taskbar.AddTab(hwnd)
        } else {
            taskbar.DeleteTab(hwnd)
        }
    }
}
