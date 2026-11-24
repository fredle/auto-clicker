//! Everything that talks to Windows: SendInput, the hidden window that owns
//! the global hotkeys and the tray icon, and the full-screen point picker.

use crate::engine::{Button, Input};
use crate::keys::{Hotkey, is_extended};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DrawTextW, EndPaint, PAINTSTRUCT, SetBkMode, SetTextColor,
    DT_CENTER, DT_SINGLELINE, DT_VCENTER, TRANSPARENT,
};
use windows::Win32::Media::timeBeginPeriod;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NOTIFY_ICON_MESSAGE, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

// ---------------------------------------------------------------- input

pub struct SystemInput;

impl SystemInput {
    pub fn new() -> Self {
        // Ask for 1 ms timer resolution so fast intervals are honoured.
        unsafe {
            timeBeginPeriod(1);
        }
        SystemInput
    }
}

fn mouse_input(flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

fn key_input(vk: u32, up: bool) -> INPUT {
    let scan = unsafe { MapVirtualKeyW(vk, MAPVK_VK_TO_VSC) } as u16;
    let mut flags = KEYEVENTF_SCANCODE;
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    if is_extended(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

fn send(inputs: &[INPUT]) {
    unsafe {
        SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

impl Input for SystemInput {
    fn cursor(&self) -> (i32, i32) {
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
        }
        (p.x, p.y)
    }

    fn move_to(&self, x: i32, y: i32) {
        unsafe {
            let _ = SetCursorPos(x, y);
        }
    }

    fn click(&self, button: Button, double: bool) {
        let (down, up) = match button {
            Button::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
            Button::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
            Button::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
        };
        let mut events = vec![mouse_input(down), mouse_input(up)];
        if double {
            events.extend([mouse_input(down), mouse_input(up)]);
        }
        send(&events);
    }

    fn key_down(&self, vk: u32) {
        send(&[key_input(vk, false)]);
    }

    fn key_up(&self, vk: u32) {
        send(&[key_input(vk, true)]);
    }
}

// ------------------------------------------------- hotkeys and tray icon

/// What the hidden window tells the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Hotkey(i32),
    TrayToggle,
    TrayShow,
    TrayQuit,
}

enum Cmd {
    Hotkeys(Vec<(i32, Hotkey)>),
    Tray(bool),
}

const WM_CMD: u32 = WM_APP + 1;
const WM_TRAY: u32 = WM_APP + 2;
const MENU_SHOW: usize = 1;
const MENU_TOGGLE: usize = 2;
const MENU_QUIT: usize = 3;

static WINDOW: AtomicIsize = AtomicIsize::new(0);
static QUEUE: Mutex<Vec<Cmd>> = Mutex::new(Vec::new());
static FAILED: Mutex<Vec<i32>> = Mutex::new(Vec::new());
static HANDLER: OnceLock<Box<dyn Fn(Event) + Send + Sync>> = OnceLock::new();

#[derive(Default)]
struct WindowState {
    registered: Vec<i32>,
    running: bool,
    taskbar_created: u32,
}

thread_local! {
    static STATE: std::cell::RefCell<WindowState> = std::cell::RefCell::new(WindowState::default());
}

fn emit(ev: Event) {
    if let Some(h) = HANDLER.get() {
        h(ev);
    }
}

/// Start the hidden window thread. `handler` runs on that thread.
pub fn start(handler: impl Fn(Event) + Send + Sync + 'static) {
    let _ = HANDLER.set(Box::new(handler));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || unsafe { window_thread(ready_tx) });
    let _ = ready_rx.recv_timeout(std::time::Duration::from_secs(5));
}

unsafe fn window_thread(ready: std::sync::mpsc::Sender<()>) {
    unsafe {
        let hinstance = HINSTANCE(GetModuleHandleW(None).unwrap_or_default().0);
        let class = w!("AutoClickerHost");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(host_proc),
            hInstance: hinstance,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("Auto Clicker host"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        ) else {
            let _ = ready.send(());
            return;
        };
        WINDOW.store(hwnd.0 as isize, Ordering::SeqCst);
        STATE.with(|s| s.borrow_mut().taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated")));
        tray(hwnd, NIM_ADD, false);
        let _ = ready.send(());
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn host_hwnd() -> Option<HWND> {
    match WINDOW.load(Ordering::SeqCst) {
        0 => None,
        v => Some(HWND(v as *mut _)),
    }
}

fn post_command(cmd: Cmd) {
    QUEUE.lock().unwrap().push(cmd);
    if let Some(hwnd) = host_hwnd() {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CMD, WPARAM(0), LPARAM(0));
        }
    }
}

/// Replace the registered global hotkeys. Ids that Windows refused (another
/// app owns the combination) show up in `failed_hotkeys` shortly after.
pub fn set_hotkeys(list: Vec<(i32, Hotkey)>) {
    post_command(Cmd::Hotkeys(list));
}

pub fn failed_hotkeys() -> Vec<i32> {
    FAILED.lock().unwrap().clone()
}

/// Tell the tray icon whether a job is running.
pub fn set_tray_running(running: bool) {
    post_command(Cmd::Tray(running));
}

/// 32x32 BGRA pixels: a target ring with a dot, green while running and grey
/// when idle, so the state is visible at a glance in the tray.
pub fn icon_pixels(running: bool) -> Vec<u8> {
    let (r, g, b) = if running { (0x2E, 0xB8, 0x4A) } else { (0x8A, 0x8F, 0x98) };
    let mut px = vec![0u8; 32 * 32 * 4];
    for y in 0..32 {
        for x in 0..32 {
            let d = (((x as f32 + 0.5 - 16.0).powi(2) + (y as f32 + 0.5 - 16.0).powi(2))).sqrt();
            let i = (y * 32 + x) * 4;
            if d <= 7.0 || (d > 11.0 && d <= 14.0) {
                px[i..i + 4].copy_from_slice(&[b, g, r, 255]);
            }
        }
    }
    px
}

unsafe fn make_icon(running: bool) -> Option<HICON> {
    let xor = icon_pixels(running);
    let and = vec![0u8; 32 * 4];
    unsafe { CreateIcon(None, 32, 32, 1, 32, and.as_ptr(), xor.as_ptr()).ok() }
}

unsafe fn tray(hwnd: HWND, op: NOTIFY_ICON_MESSAGE, running: bool) {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        ..Default::default()
    };
    if let Some(icon) = unsafe { make_icon(running) } {
        nid.hIcon = icon;
    }
    let tip = if running { "Auto Clicker - running" } else { "Auto Clicker - idle" };
    for (i, c) in tip.encode_utf16().enumerate().take(127) {
        nid.szTip[i] = c;
    }
    unsafe {
        let _ = Shell_NotifyIconW(op, &nid);
        if !nid.hIcon.is_invalid() {
            let _ = DestroyIcon(nid.hIcon);
        }
    }
}

unsafe fn apply_hotkeys(hwnd: HWND, list: Vec<(i32, Hotkey)>) {
    let mut failed = Vec::new();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        for id in s.registered.drain(..) {
            unsafe {
                let _ = UnregisterHotKey(Some(hwnd), id);
            }
        }
        for (id, hk) in list {
            let ok = unsafe {
                RegisterHotKey(Some(hwnd), id, HOT_KEY_MODIFIERS(hk.mods) | MOD_NOREPEAT, hk.vk)
            }
            .is_ok();
            if ok {
                s.registered.push(id);
            } else {
                failed.push(id);
            }
        }
    });
    *FAILED.lock().unwrap() = failed;
}

unsafe fn show_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let running = STATE.with(|s| s.borrow().running);
        let _ = AppendMenuW(menu, MF_STRING, MENU_SHOW, w!("Show Auto Clicker"));
        let toggle = if running { w!("Stop") } else { w!("Start") };
        let _ = AppendMenuW(menu, MF_STRING, MENU_TOGGLE, toggle);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Quit"));
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let _ = SetForegroundWindow(hwnd);
        let picked = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, p.x, p.y, None, hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        match picked.0 as usize {
            MENU_SHOW => emit(Event::TrayShow),
            MENU_TOGGLE => emit(Event::TrayToggle),
            MENU_QUIT => emit(Event::TrayQuit),
            _ => {}
        }
    }
}

unsafe extern "system" fn host_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CMD => {
                let cmds: Vec<Cmd> = std::mem::take(&mut *QUEUE.lock().unwrap());
                for cmd in cmds {
                    match cmd {
                        Cmd::Hotkeys(list) => apply_hotkeys(hwnd, list),
                        Cmd::Tray(running) => {
                            STATE.with(|s| s.borrow_mut().running = running);
                            tray(hwnd, NIM_MODIFY, running);
                        }
                    }
                }
                LRESULT(0)
            }
            WM_HOTKEY => {
                emit(Event::Hotkey(wparam.0 as i32));
                LRESULT(0)
            }
            WM_TRAY => {
                match (lparam.0 & 0xFFFF) as u32 {
                    WM_LBUTTONUP | WM_LBUTTONDBLCLK => emit(Event::TrayShow),
                    WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                tray(hwnd, NIM_DELETE, false);
                LRESULT(0)
            }
            _ => {
                let created = STATE.with(|s| s.borrow().taskbar_created);
                if created != 0 && msg == created {
                    let running = STATE.with(|s| s.borrow().running);
                    tray(hwnd, NIM_ADD, running);
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
    }
}

/// Bring the app's main window back from the taskbar or behind others.
pub fn show_main_window() {
    unsafe {
        if let Ok(hwnd) = FindWindowW(PCWSTR::null(), w!("Auto Clicker - Mouse & Key Presser")) {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

// ----------------------------------------------------------- point picker

static PICKED: Mutex<Option<(i32, i32)>> = Mutex::new(None);

/// Show a dimmed full-screen overlay and return the point the user clicks,
/// or None if they cancel (Esc or right click). Blocks until done.
pub fn pick_point() -> Option<(i32, i32)> {
    *PICKED.lock().unwrap() = None;
    unsafe {
        let hinstance = HINSTANCE(GetModuleHandleW(None).unwrap_or_default().0);
        let class = w!("AutoClickerPicker");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(picker_proc),
            hInstance: hinstance,
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
            hbrBackground: CreateSolidBrush(COLORREF(0)),
            ..Default::default()
        };
        RegisterClassW(&wc); // fails harmlessly if already registered
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class,
            w!("Pick a point"),
            WS_POPUP,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(hinstance),
            None,
        )
        .ok()?;
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 150, LWA_ALPHA);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    PICKED.lock().unwrap().take()
}

unsafe extern "system" fn picker_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_LBUTTONDOWN => {
                let mut p = POINT::default();
                let _ = GetCursorPos(&mut p);
                *PICKED.lock().unwrap() = Some((p.x, p.y));
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_RBUTTONDOWN => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_KEYDOWN if wparam.0 as u16 == VK_ESCAPE.0 => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, COLORREF(0x00FF_FFFF));
                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);
                let mut text: Vec<u16> =
                    "Click the spot to use.  Esc or right-click cancels.".encode_utf16().collect();
                DrawTextW(hdc, &mut text, &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_icon_differs_between_running_and_idle() {
        let idle = icon_pixels(false);
        let running = icon_pixels(true);
        assert_eq!(idle.len(), 32 * 32 * 4);
        let centre = (16 * 32 + 16) * 4;
        assert_eq!(idle[centre + 3], 255); // dot drawn in the middle either way
        assert_eq!(&running[centre..centre + 3], &[0x4A, 0xB8, 0x2E]);
        assert_eq!(&idle[centre..centre + 3], &[0x98, 0x8F, 0x8A]);
        assert_ne!(idle, running);
        assert_eq!(idle[3], 0); // corners stay transparent
    }
}
