use windows::Win32::UI::WindowsAndMessaging::{
    HICON, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTCOLOR, LoadIconW, LoadImageW,
};
use windows::core::PCWSTR;

const APP_ICON_RESOURCE_ID: usize = 1;

fn app_icon_resource() -> PCWSTR {
    PCWSTR(std::ptr::without_provenance::<u16>(APP_ICON_RESOURCE_ID))
}

pub fn load_app_icon() -> windows::core::Result<HICON> {
    unsafe {
        let module = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        LoadIconW(Some(module.into()), app_icon_resource())
            .or_else(|_| LoadIconW(None, IDI_APPLICATION))
    }
}

pub fn load_app_icon_sized(width: i32, height: i32) -> windows::core::Result<HICON> {
    unsafe {
        let module = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        LoadImageW(
            Some(module.into()),
            app_icon_resource(),
            IMAGE_ICON,
            width,
            height,
            LR_DEFAULTCOLOR,
        )
        .map(|handle| HICON(handle.0))
    }
}
