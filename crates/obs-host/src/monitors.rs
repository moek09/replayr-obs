//! The monitors in the order the app numbers them (`Source::Monitor { index }`):
//! DXGI adapter and output order, attached to the desktop, the primary one
//! first. Must stay the same order as the app's own monitor list.

/// GDI device names (`\\.\DISPLAY1`, ...), the primary one first; the rest
/// keep their order.
pub fn primary_first(monitors: Vec<(String, bool)>) -> Vec<String> {
    let mut monitors = monitors;
    monitors.sort_by_key(|(_, primary)| !primary);
    monitors.into_iter().map(|(name, _)| name).collect()
}

#[cfg(windows)]
pub fn monitor_names() -> Vec<String> {
    use windows::core::Interface;
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput1};
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};
    use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;

    let mut list = Vec::new();
    // SAFETY: plain DXGI enumeration calls.
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
            return Vec::new();
        };
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            a += 1;
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                o += 1;
                // Same filter as the app: outputs without IDXGIOutput1 are skipped.
                let (Ok(desc), Ok(_)) = (output.GetDesc(), output.cast::<IDXGIOutput1>()) else {
                    continue;
                };
                if !desc.AttachedToDesktop.as_bool() {
                    continue;
                }
                let mut info = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                let primary = GetMonitorInfoW(desc.Monitor, &mut info).as_bool()
                    && info.dwFlags & MONITORINFOF_PRIMARY != 0;
                let len = desc
                    .DeviceName
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(desc.DeviceName.len());
                list.push((String::from_utf16_lossy(&desc.DeviceName[..len]), primary));
            }
        }
    }
    primary_first(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[(&str, bool)]) -> Vec<String> {
        primary_first(list.iter().map(|(n, p)| (n.to_string(), *p)).collect())
    }

    #[test]
    fn the_primary_monitor_comes_first_and_the_rest_keep_their_order() {
        assert_eq!(
            names(&[("D1", false), ("D2", false), ("D3", true), ("D4", false)]),
            ["D3", "D1", "D2", "D4"]
        );
    }

    #[test]
    fn without_a_primary_the_order_stays() {
        assert_eq!(names(&[("D2", false), ("D1", false)]), ["D2", "D1"]);
        assert!(names(&[]).is_empty());
    }
}
