#![cfg(windows)]
#![cfg_attr(test, allow(dead_code))]

use crate::workspace::Workspace;
use rune::AppHandle;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CREATESTRUCTW, CW_USEDEFAULT, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetCursorPos,
    GetMessageW, GetWindowLongPtrW, HICON, IMAGE_ICON, LR_DEFAULTSIZE, LR_SHARED, LoadIconW,
    LoadImageW, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassExW,
    RegisterWindowMessageW, SetForegroundWindow, SetWindowLongPtrW, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CLOSE, WM_CONTEXTMENU,
    WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NCCREATE, WM_NULL, WM_RBUTTONUP, WNDCLASSEXW,
    WS_OVERLAPPED,
};

const WM_TRAY: u32 = WM_APP + 1;
const TRAY_UID: u32 = 1;
const CMD_SHOW: u32 = 1001;
const CMD_QUIT: u32 = 1002;

struct TrayState {
    handle: AppHandle<Workspace>,
    icon: HICON,
    owns_icon: bool,
    taskbar_created: u32,
}

pub struct Tray {
    hwnd: usize,
    thread: Option<JoinHandle<()>>,
}

impl Tray {
    pub fn start(handle: AppHandle<Workspace>) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("mcli-tray".into())
            .spawn(move || run_tray_loop(handle, tx))
            .map_err(|e| format!("无法启动托盘线程：{e}"))?;
        match rx.recv() {
            Ok(Ok(hwnd)) => Ok(Self {
                hwnd,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err("托盘线程提前退出".into())
            }
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if self.hwnd != 0 {
            unsafe {
                PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0);
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text).encode_wide().chain(Some(0)).collect()
}

fn make_notify_data(hwnd: HWND, icon: HICON) -> NOTIFYICONDATAW {
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = TRAY_UID;
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = icon;
    let tip = wide("Mcli · 多 Agent 终端工作台");
    let copy_len = tip.len().min(data.szTip.len() - 1);
    data.szTip[..copy_len].copy_from_slice(&tip[..copy_len]);
    data
}

fn load_tray_icon(instance: windows_sys::Win32::Foundation::HMODULE) -> (HICON, bool) {
    unsafe {
        let icon = LoadImageW(
            instance,
            1 as *const u16,
            IMAGE_ICON,
            16,
            16,
            LR_DEFAULTSIZE,
        ) as HICON;
        if !icon.is_null() {
            return (icon, true);
        }
        let shared = LoadImageW(
            instance,
            1 as *const u16,
            IMAGE_ICON,
            0,
            0,
            LR_DEFAULTSIZE | LR_SHARED,
        ) as HICON;
        if !shared.is_null() {
            return (shared, false);
        }
        let fallback = LoadIconW(null_mut(), 32512 as *const u16); // IDI_APPLICATION
        (fallback, false)
    }
}

fn run_tray_loop(handle: AppHandle<Workspace>, ready: mpsc::Sender<Result<usize, String>>) {
    unsafe {
        let instance = GetModuleHandleW(null());
        let class_name = wide("McliTrayWindowClass");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(tray_wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: null_mut(),
            hCursor: null_mut(),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class_name.as_ptr(),
            hIconSm: null_mut(),
        };
        RegisterClassExW(&wc);

        let (icon, owns_icon) = load_tray_icon(instance);
        let taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        let state = Box::new(TrayState {
            handle,
            icon,
            owns_icon,
            taskbar_created,
        });
        let state_ptr = Box::into_raw(state);

        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            wide("McliTray").as_ptr(),
            WS_OVERLAPPED,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            state_ptr.cast(),
        );
        if hwnd.is_null() {
            let state = Box::from_raw(state_ptr);
            if state.owns_icon && !state.icon.is_null() {
                DestroyIcon(state.icon);
            }
            let _ = ready.send(Err("无法创建托盘消息窗口".into()));
            return;
        }

        let nid = make_notify_data(hwnd, (*state_ptr).icon);
        if Shell_NotifyIconW(NIM_ADD, &nid) == 0 {
            DestroyWindow(hwnd);
            let _ = ready.send(Err("无法注册系统托盘图标".into()));
            return;
        }

        let _ = ready.send(Ok(hwnd as usize));

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe fn show_context_menu(hwnd: HWND, handle: &AppHandle<Workspace>) {
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        AppendMenuW(
            menu,
            MF_STRING,
            CMD_SHOW as usize,
            wide("显示 Mcli").as_ptr(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        AppendMenuW(
            menu,
            MF_STRING,
            CMD_QUIT as usize,
            wide("退出 Mcli").as_ptr(),
        );

        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_NONOTIFY | TPM_RETURNCMD,
            pt.x,
            pt.y,
            0,
            hwnd,
            null(),
        ) as u32;
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);

        match cmd {
            CMD_SHOW => handle.open(),
            CMD_QUIT => handle.quit(),
            _ => (),
        }
    }
}

unsafe extern "system" fn tray_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let cs = lparam as *const CREATESTRUCTW;
            if !cs.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize);
            }
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }

        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TrayState;
        if !ptr.is_null() {
            let state = &*ptr;
            if msg == WM_TRAY {
                let event = (lparam as u32) & 0xffff;
                match event {
                    WM_LBUTTONUP | WM_LBUTTONDBLCLK => state.handle.open(),
                    WM_RBUTTONUP | WM_CONTEXTMENU => show_context_menu(hwnd, &state.handle),
                    _ => (),
                }
                return 0;
            }
            if state.taskbar_created != 0 && msg == state.taskbar_created {
                let nid = make_notify_data(hwnd, state.icon);
                Shell_NotifyIconW(NIM_ADD, &nid);
                return 0;
            }
        }

        match msg {
            WM_CLOSE => {
                if !ptr.is_null() {
                    let nid = make_notify_data(hwnd, (*ptr).icon);
                    Shell_NotifyIconW(NIM_DELETE, &nid);
                }
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                if !ptr.is_null() {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    let state = Box::from_raw(ptr);
                    if state.owns_icon && !state.icon.is_null() {
                        DestroyIcon(state.icon);
                    }
                }
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
