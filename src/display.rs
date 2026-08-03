#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorInfo {
    pub output_index: u32,
    pub id: String,
    pub label: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

impl MonitorInfo {
    pub fn display_label(&self) -> String {
        format!(
            "{} · {}×{}{}",
            self.label,
            self.width,
            self.height,
            if self.primary { " · Primary" } else { "" }
        )
    }
}

#[cfg(windows)]
pub fn enumerate_monitors() -> Vec<MonitorInfo> {
    use std::{mem::size_of, ptr};
    use windows_sys::{
        Win32::{
            Foundation::{LPARAM, RECT},
            Graphics::Gdi::{
                EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
            },
            UI::WindowsAndMessaging::MONITORINFOF_PRIMARY,
        },
        core::BOOL,
    };

    unsafe extern "system" fn callback(
        monitor: HMONITOR,
        _dc: HDC,
        _rect: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let displays = unsafe { &mut *(data as *mut Vec<MonitorInfo>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if unsafe {
            GetMonitorInfoW(
                monitor,
                (&mut info as *mut MONITORINFOEXW).cast::<MONITORINFO>(),
            )
        } != 0
        {
            let rect = info.monitorInfo.rcMonitor;
            let name_len = info
                .szDevice
                .iter()
                .position(|character| *character == 0)
                .unwrap_or(info.szDevice.len());
            let id = String::from_utf16_lossy(&info.szDevice[..name_len]);
            displays.push(MonitorInfo {
                output_index: displays.len() as u32,
                label: id.replace("\\\\.\\", ""),
                id,
                x: rect.left,
                y: rect.top,
                width: (rect.right - rect.left).max(1) as u32,
                height: (rect.bottom - rect.top).max(1) as u32,
                primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
        1
    }

    let mut displays: Vec<MonitorInfo> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            ptr::null_mut(),
            ptr::null(),
            Some(callback),
            (&mut displays as *mut Vec<MonitorInfo>) as LPARAM,
        );
    }
    displays.sort_by_key(|monitor| (!monitor.primary, monitor.x, monitor.y));
    displays
}

#[cfg(not(windows))]
pub fn enumerate_monitors() -> Vec<MonitorInfo> {
    vec![MonitorInfo {
        output_index: 0,
        id: "primary".into(),
        label: "Primary display".into(),
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
        primary: true,
    }]
}

pub fn selected_monitor(id: Option<&str>) -> Option<MonitorInfo> {
    let monitors = enumerate_monitors();
    id.and_then(|selected| {
        monitors
            .iter()
            .find(|monitor| monitor.id == selected)
            .cloned()
    })
    .or_else(|| monitors.iter().find(|monitor| monitor.primary).cloned())
    .or_else(|| monitors.into_iter().next())
}
