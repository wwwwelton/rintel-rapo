use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

pub struct TrayController {
    pub minimize_to_tray: Arc<AtomicBool>,
    pub optimizer_running: Arc<AtomicBool>,
    stop_flag: Arc<AtomicBool>,
}

#[cfg(windows)]
mod win_tray {
    use std::mem::{size_of, zeroed};
    use std::sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc,
    };
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        UI::WindowsAndMessaging::{
            AppendMenuW, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
            DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, EnumWindows, GetCursorPos,
            GetWindowThreadProcessId, IsIconic, IsWindowVisible, PeekMessageW, PostMessageW,
            SetForegroundWindow, ShowWindow, TrackPopupMenu, TranslateMessage, CW_USEDEFAULT,
            HICON, MF_CHECKED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, MSG, PM_REMOVE, SW_HIDE,
            SW_RESTORE, SW_SHOW, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON,
            WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_PAINT,
            WM_RBUTTONUP,
        },
    };

    const WM_TRAYICON: u32 = WM_APP + 42;
    const TRAY_UID: u32 = 1001;

    const CMD_START: usize = 1;
    const CMD_STOP: usize = 2;
    const CMD_RESTORE: usize = 3;
    const CMD_EXIT: usize = 4;

    const NIM_ADD: u32 = 0x0000_0000;
    const NIM_MODIFY: u32 = 0x0000_0001;
    const NIM_DELETE: u32 = 0x0000_0002;
    const NIF_MESSAGE: u32 = 0x0000_0001;
    const NIF_ICON: u32 = 0x0000_0002;
    const NIF_TIP: u32 = 0x0000_0004;

    // 1 = Left click (Restore), 2 = Right click (Context Menu)
    static PENDING_TRAY_EVENT: AtomicU32 = AtomicU32::new(0);

    #[repr(C)]
    struct WndClassW {
        style: u32,
        lpfn_wnd_proc: Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>,
        cb_cls_extra: i32,
        cb_wnd_extra: i32,
        h_instance: *mut std::ffi::c_void,
        h_icon: HICON,
        h_cursor: *mut std::ffi::c_void,
        hbr_background: *mut std::ffi::c_void,
        lpsz_menu_name: *const u16,
        lpsz_class_name: *const u16,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NotifyIconDataW {
        cb_size: u32,
        h_wnd: HWND,
        u_id: u32,
        u_flags: u32,
        u_callback_message: u32,
        h_icon: HICON,
        sz_tip: [u16; 128],
    }

    #[link(name = "user32")]
    extern "system" {
        fn RegisterClassW(lp_wnd_class: *const WndClassW) -> u16;
    }

    #[link(name = "shell32")]
    extern "system" {
        fn Shell_NotifyIconW(dw_message: u32, lp_data: *const NotifyIconDataW) -> i32;
    }

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn fill_tip(buf: &mut [u16; 128], text: &str) {
        *buf = [0u16; 128];
        for (i, ch) in text.encode_utf16().take(127).enumerate() {
            buf[i] = ch;
        }
    }

    /// Generates a 32x32 32-bit BGRA HICON in memory (Active = Mint/Cyan glow, Stopped = Amber/Slate)
    pub unsafe fn create_status_icon(active: bool) -> HICON {
        let width = 32i32;
        let height = 32i32;
        let and_mask = [0u8; (32 * 32) / 8];
        let mut xor_pixels = [0u8; 32 * 32 * 4]; // BGRA

        for y in 0..height {
            for x in 0..width {
                let dx = x as f32 - 15.5;
                let dy = y as f32 - 15.5;
                let dist = (dx * dx + dy * dy).sqrt();
                let idx = ((y * width + x) * 4) as usize;

                if dist <= 14.5 {
                    if active {
                        // Active: NotMeter Mint/Cyan (#46e0d5) ring + glowing core (#55d7ff)
                        if dist >= 11.5 {
                            xor_pixels[idx] = 213; // B
                            xor_pixels[idx + 1] = 224; // G
                            xor_pixels[idx + 2] = 70; // R
                            xor_pixels[idx + 3] = 255; // A
                        } else if dist <= 5.5 {
                            xor_pixels[idx] = 255; // B
                            xor_pixels[idx + 1] = 235; // G
                            xor_pixels[idx + 2] = 110; // R
                            xor_pixels[idx + 3] = 255; // A
                        } else {
                            xor_pixels[idx] = 39; // B (#0c1a27)
                            xor_pixels[idx + 1] = 26; // G
                            xor_pixels[idx + 2] = 12; // R
                            xor_pixels[idx + 3] = 245; // A
                        }
                    } else {
                        // Stopped/Paused: Amber/Slate ring (#ffd96b) + pause bars
                        let is_pause_bar =
                            (y >= 10 && y <= 21) && ((x >= 11 && x <= 14) || (x >= 17 && x <= 20));
                        if dist >= 11.5 || is_pause_bar {
                            xor_pixels[idx] = 107; // B
                            xor_pixels[idx + 1] = 217; // G
                            xor_pixels[idx + 2] = 255; // R
                            xor_pixels[idx + 3] = 255; // A
                        } else {
                            xor_pixels[idx] = 35; // B
                            xor_pixels[idx + 1] = 24; // G
                            xor_pixels[idx + 2] = 16; // R
                            xor_pixels[idx + 3] = 245; // A
                        }
                    }
                }
            }
        }

        CreateIcon(
            std::ptr::null_mut(),
            width,
            height,
            1,
            32,
            and_mask.as_ptr(),
            xor_pixels.as_ptr(),
        )
    }

    struct EnumCtx {
        target_pid: u32,
        tray_hwnd: HWND,
        found_hwnd: HWND,
    }

    unsafe extern "system" fn enum_windows_proc(hwnd: HWND, lparam: LPARAM) -> i32 {
        let ctx = &mut *(lparam as *mut EnumCtx);
        if hwnd == ctx.tray_hwnd {
            return 1;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == ctx.target_pid {
            let mut title = [0u16; 64];
            let len = windows_sys::Win32::UI::WindowsAndMessaging::GetWindowTextW(
                hwnd,
                title.as_mut_ptr(),
                title.len() as i32,
            );
            if len > 0 {
                let s = String::from_utf16_lossy(&title[..len as usize]);
                if s.contains("Rintel RAPO") {
                    ctx.found_hwnd = hwnd;
                    return 0;
                }
            }
        }
        1
    }

    unsafe fn find_main_window(tray_hwnd: HWND) -> HWND {
        let pid = windows_sys::Win32::System::Threading::GetCurrentProcessId();
        let mut ctx = EnumCtx {
            target_pid: pid,
            tray_hwnd,
            found_hwnd: std::ptr::null_mut(),
        };
        EnumWindows(Some(enum_windows_proc), (&mut ctx as *mut EnumCtx) as isize);
        ctx.found_hwnd
    }

    unsafe fn restore_main_window(main_hwnd: HWND) {
        if !main_hwnd.is_null() {
            ShowWindow(main_hwnd, SW_SHOW);
            ShowWindow(main_hwnd, SW_RESTORE);
            SetForegroundWindow(main_hwnd);
            PostMessageW(main_hwnd, WM_PAINT, 0, 0);
        }
    }

    unsafe extern "system" fn tray_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_TRAYICON {
            let event = (lparam as u32) & 0xFFFF;
            if event == WM_LBUTTONUP || event == WM_LBUTTONDBLCLK {
                PENDING_TRAY_EVENT.store(1, Ordering::Relaxed);
            } else if event == WM_RBUTTONUP || event == WM_CONTEXTMENU {
                PENDING_TRAY_EVENT.store(2, Ordering::Relaxed);
            }
            return 0;
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    pub fn spawn_tray_thread(
        minimize_to_tray: Arc<AtomicBool>,
        optimizer_running: Arc<AtomicBool>,
        stop_flag: Arc<AtomicBool>,
    ) {
        std::thread::spawn(move || unsafe {
            let class_name = to_wide("RintelRapoTrayClass");
            let wc = WndClassW {
                style: 0,
                lpfn_wnd_proc: Some(tray_wnd_proc),
                cb_cls_extra: 0,
                cb_wnd_extra: 0,
                h_instance: std::ptr::null_mut(),
                h_icon: std::ptr::null_mut(),
                h_cursor: std::ptr::null_mut(),
                hbr_background: std::ptr::null_mut(),
                lpsz_menu_name: std::ptr::null(),
                lpsz_class_name: class_name.as_ptr(),
            };
            let _ = RegisterClassW(&wc);

            let tray_hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                to_wide("RintelRapoTray").as_ptr(),
                0,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            if tray_hwnd.is_null() {
                return;
            }

            let icon_active = create_status_icon(true);
            let icon_paused = create_status_icon(false);
            let mut last_active_state = optimizer_running.load(Ordering::Relaxed);

            let mut nid = NotifyIconDataW {
                cb_size: size_of::<NotifyIconDataW>() as u32,
                h_wnd: tray_hwnd,
                u_id: TRAY_UID,
                u_flags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
                u_callback_message: WM_TRAYICON,
                h_icon: if last_active_state { icon_active } else { icon_paused },
                sz_tip: [0u16; 128],
            };
            fill_tip(
                &mut nid.sz_tip,
                if last_active_state {
                    "Rintel RAPO — Otimizador Ativo (Clique para abrir)"
                } else {
                    "Rintel RAPO — Otimizador Pausado (Clique para abrir)"
                },
            );
            let _ = Shell_NotifyIconW(NIM_ADD, &nid);

            let mut main_hwnd: HWND = std::ptr::null_mut();
            let mut hidden_in_tray = false;
            let mut tick_counter = 0u32;

            while !stop_flag.load(Ordering::Relaxed) {
                if main_hwnd.is_null() {
                    main_hwnd = find_main_window(tray_hwnd);
                }

                // 1. Update tray icon immediately if active/paused state changed
                let current_active = optimizer_running.load(Ordering::Relaxed);
                if current_active != last_active_state {
                    last_active_state = current_active;
                    nid.h_icon = if current_active { icon_active } else { icon_paused };
                    fill_tip(
                        &mut nid.sz_tip,
                        if current_active {
                            "Rintel RAPO — Otimizador Ativo (Clique para abrir)"
                        } else {
                            "Rintel RAPO — Otimizador Pausado (Clique para abrir)"
                        },
                    );
                    let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
                }

                // 2. Check if main window was minimized and should hide to tray
                if !main_hwnd.is_null() {
                    if minimize_to_tray.load(Ordering::Relaxed) {
                        if IsIconic(main_hwnd) != 0 && IsWindowVisible(main_hwnd) != 0 {
                            ShowWindow(main_hwnd, SW_HIDE);
                            hidden_in_tray = true;
                        }
                    } else if hidden_in_tray {
                        restore_main_window(main_hwnd);
                        hidden_in_tray = false;
                    }

                    // Keep background optimization loop ticking once per second while hidden in tray
                    tick_counter = tick_counter.wrapping_add(1);
                    if hidden_in_tray && tick_counter % 20 == 0 {
                        PostMessageW(main_hwnd, WM_PAINT, 0, 0);
                    }
                }

                // 3. Pump Tray Window Messages
                let mut msg: MSG = zeroed();
                while PeekMessageW(&mut msg, tray_hwnd, 0, 0, PM_REMOVE) != 0 {
                    if msg.message == WM_TRAYICON {
                        let event = (msg.lParam as u32) & 0xFFFF;
                        if event == WM_LBUTTONUP || event == WM_LBUTTONDBLCLK {
                            PENDING_TRAY_EVENT.store(1, Ordering::Relaxed);
                        } else if event == WM_RBUTTONUP || event == WM_CONTEXTMENU {
                            PENDING_TRAY_EVENT.store(2, Ordering::Relaxed);
                        }
                    }
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }

                // 4. Handle any triggered Tray Click / Context Menu event
                let action = PENDING_TRAY_EVENT.swap(0, Ordering::Relaxed);
                if action == 1 {
                    if main_hwnd.is_null() {
                        main_hwnd = find_main_window(tray_hwnd);
                    }
                    restore_main_window(main_hwnd);
                    hidden_in_tray = false;
                } else if action == 2 {
                    let menu = CreatePopupMenu();
                    if !menu.is_null() {
                        let is_running = optimizer_running.load(Ordering::Relaxed);
                        let start_str = to_wide("▶ Start — Ativar Otimizador");
                        let stop_str = to_wide("⏹ Stop — Pausar Otimizador");
                        let restore_str = to_wide("🗔 Abrir / Restaurar Rintel RAPO");
                        let exit_str = to_wide("✖ Sair do Programa");

                        let _ = AppendMenuW(
                            menu,
                            MF_STRING | if is_running { MF_CHECKED } else { MF_UNCHECKED },
                            CMD_START,
                            start_str.as_ptr(),
                        );
                        let _ = AppendMenuW(
                            menu,
                            MF_STRING | if !is_running { MF_CHECKED } else { MF_UNCHECKED },
                            CMD_STOP,
                            stop_str.as_ptr(),
                        );
                        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                        let _ = AppendMenuW(menu, MF_STRING, CMD_RESTORE, restore_str.as_ptr());
                        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                        let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, exit_str.as_ptr());

                        let mut pt = POINT { x: 0, y: 0 };
                        GetCursorPos(&mut pt);
                        SetForegroundWindow(tray_hwnd);
                        let cmd = TrackPopupMenu(
                            menu,
                            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_LEFTALIGN,
                            pt.x,
                            pt.y,
                            0,
                            tray_hwnd,
                            std::ptr::null(),
                        ) as usize;
                        DestroyMenu(menu);

                        match cmd {
                            CMD_START => {
                                optimizer_running.store(true, Ordering::Relaxed);
                                if !main_hwnd.is_null() {
                                    PostMessageW(main_hwnd, WM_PAINT, 0, 0);
                                }
                            }
                            CMD_STOP => {
                                optimizer_running.store(false, Ordering::Relaxed);
                                if !main_hwnd.is_null() {
                                    PostMessageW(main_hwnd, WM_PAINT, 0, 0);
                                }
                            }
                            CMD_RESTORE => {
                                if main_hwnd.is_null() {
                                    main_hwnd = find_main_window(tray_hwnd);
                                }
                                restore_main_window(main_hwnd);
                                hidden_in_tray = false;
                            }
                            CMD_EXIT => {
                                if main_hwnd.is_null() {
                                    main_hwnd = find_main_window(tray_hwnd);
                                }
                                if !main_hwnd.is_null() {
                                    ShowWindow(main_hwnd, SW_SHOW);
                                    PostMessageW(main_hwnd, WM_CLOSE, 0, 0);
                                }
                                stop_flag.store(true, Ordering::Relaxed);
                            }
                            _ => {}
                        }
                    }
                }

                std::thread::sleep(std::time::Duration::from_millis(50));
            }

            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            if !icon_active.is_null() {
                DestroyIcon(icon_active);
            }
            if !icon_paused.is_null() {
                DestroyIcon(icon_paused);
            }
            DestroyWindow(tray_hwnd);
        });
    }
}

impl TrayController {
    pub fn new() -> Self {
        let minimize_to_tray = Arc::new(AtomicBool::new(true));
        let optimizer_running = Arc::new(AtomicBool::new(true));
        let stop_flag = Arc::new(AtomicBool::new(false));

        #[cfg(windows)]
        win_tray::spawn_tray_thread(
            Arc::clone(&minimize_to_tray),
            Arc::clone(&optimizer_running),
            Arc::clone(&stop_flag),
        );

        Self {
            minimize_to_tray,
            optimizer_running,
            stop_flag,
        }
    }

    pub fn is_minimize_to_tray(&self) -> bool {
        self.minimize_to_tray.load(Ordering::Relaxed)
    }

    pub fn set_minimize_to_tray(&self, val: bool) {
        self.minimize_to_tray.store(val, Ordering::Relaxed);
    }

    pub fn is_optimizer_running(&self) -> bool {
        self.optimizer_running.load(Ordering::Relaxed)
    }

    pub fn set_optimizer_running(&self, val: bool) {
        self.optimizer_running.store(val, Ordering::Relaxed);
    }
}

impl Drop for TrayController {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
    }
}

/// Generates an RGBA 32x32 icon for the main eframe window (active = Mint/Cyan, paused = Amber)
pub fn build_window_icon_rgba(active: bool) -> (Vec<u8>, u32, u32) {
    let width = 32u32;
    let height = 32u32;
    let mut rgba = vec![0u8; (width * height * 4) as usize];

    for y in 0..height {
        for x in 0..width {
            let dx = x as f32 - 15.5;
            let dy = y as f32 - 15.5;
            let dist = (dx * dx + dy * dy).sqrt();
            let idx = ((y * width + x) * 4) as usize;

            if dist <= 14.5 {
                if active {
                    if dist >= 11.5 {
                        rgba[idx] = 70; // R (#46e0d5)
                        rgba[idx + 1] = 224; // G
                        rgba[idx + 2] = 213; // B
                        rgba[idx + 3] = 255; // A
                    } else if dist <= 5.5 {
                        rgba[idx] = 85; // R (#55d7ff)
                        rgba[idx + 1] = 215; // G
                        rgba[idx + 2] = 255; // B
                        rgba[idx + 3] = 255; // A
                    } else {
                        rgba[idx] = 12; // R (#0c1a27)
                        rgba[idx + 1] = 26; // G
                        rgba[idx + 2] = 39; // B
                        rgba[idx + 3] = 245; // A
                    }
                } else {
                    let is_pause_bar =
                        (y >= 10 && y <= 21) && ((x >= 11 && x <= 14) || (x >= 17 && x <= 20));
                    if dist >= 11.5 || is_pause_bar {
                        rgba[idx] = 255; // R (#ffd96b)
                        rgba[idx + 1] = 217; // G
                        rgba[idx + 2] = 107; // B
                        rgba[idx + 3] = 255; // A
                    } else {
                        rgba[idx] = 16; // R
                        rgba[idx + 1] = 24; // G
                        rgba[idx + 2] = 35; // B
                        rgba[idx + 3] = 245; // A
                    }
                }
            }
        }
    }

    (rgba, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tray_controller_state_and_icons() {
        let tray = TrayController::new();
        assert!(tray.is_minimize_to_tray());
        assert!(tray.is_optimizer_running());

        tray.set_optimizer_running(false);
        assert!(!tray.is_optimizer_running());

        tray.set_minimize_to_tray(false);
        assert!(!tray.is_minimize_to_tray());

        let (active_rgba, w, h) = build_window_icon_rgba(true);
        let (paused_rgba, _, _) = build_window_icon_rgba(false);
        assert_eq!(w, 32);
        assert_eq!(h, 32);
        assert_eq!(active_rgba.len(), 32 * 32 * 4);
        assert_ne!(active_rgba, paused_rgba, "Active and paused icons must differ visually");
    }
}
