//! System notifications and sounds for alerts on new lines, without extra libraries: on
//! Windows a notification-area icon's balloon (shown as a notification in the Action Center,
//! with BigText's name and icon; a click comes back to BigText), on Linux `notify-send` or the
//! notification service over `gdbus`, on macOS `osascript`. Sounds: the notification's own, or
//! the system's warning sound alone.

use std::process::{Command, Stdio};
use std::sync::Mutex;

/// The self-test keeps notifications here instead of showing them.
static CAPTURED: Mutex<Option<Vec<(String, String, bool)>>> = Mutex::new(None);

/// Self-test: notifications are kept, not shown.
pub fn capture() {
    *CAPTURED.lock().unwrap() = Some(Vec::new());
}

/// Self-test: the notifications kept so far (title, text, sound).
pub fn captured() -> Vec<(String, String, bool)> {
    CAPTURED.lock().unwrap().clone().unwrap_or_default()
}

/// Shows a notification (playing its sound when `sound`); returns at once, `false` when the
/// system did not take it (no notification service or tool).
pub fn show(title: &str, body: &str, sound: bool) -> bool {
    if let Some(v) = CAPTURED.lock().unwrap().as_mut() {
        v.push((title.to_string(), body.to_string(), sound));
        return true;
    }
    os::show(title, body, sound)
}

/// Plays the system's warning sound.
pub fn beep() {
    if let Some(v) = CAPTURED.lock().unwrap().as_mut() {
        v.push((String::new(), String::new(), true));
        return;
    }
    os::beep();
}

/// What a click on a notification does (Windows; elsewhere the system decides).
pub fn on_click(f: Box<dyn Fn() + Send + Sync>) {
    os::on_click(f);
}

/// Takes the notification-area icon away (when no tab has alerts any more, and at exit).
pub fn hide() {
    os::hide();
}

/// Starts `program args` without a console window and without waiting for it; `false` when it
/// is not installed.
#[allow(dead_code)]
fn spawn(program: &str, args: &[&str]) -> bool {
    let mut c = Command::new(program);
    c.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    bigtext::remote::hide_console(&mut c);
    match c.spawn() {
        Ok(mut child) => {
            // Reaped, so no zombie is left.
            std::thread::spawn(move || child.wait());
            true
        }
        Err(_) => false,
    }
}

#[cfg(target_os = "linux")]
mod os {
    use super::spawn;

    /// A string in GVariant's text form.
    fn gvariant(s: &str) -> String {
        format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
    }

    pub fn show(title: &str, body: &str, sound: bool) -> bool {
        let mut args = vec!["-a", "BigText", "-i", "bigtext"];
        if sound {
            args.extend(["-h", "string:sound-name:dialog-warning"]);
        }
        args.extend([title, body]);
        if spawn("notify-send", &args) {
            return true;
        }
        let hints = if sound {
            "{'sound-name': <'dialog-warning'>}"
        } else {
            "{}"
        };
        let (t, b) = (gvariant(title), gvariant(body));
        let args = [
            "call",
            "--session",
            "--dest=org.freedesktop.Notifications",
            "--object-path=/org/freedesktop/Notifications",
            "--method=org.freedesktop.Notifications.Notify",
            "'BigText'",
            "0",
            "'bigtext'",
            t.as_str(),
            b.as_str(),
            "[]",
            hints,
            "8000",
        ];
        let shown = spawn("gdbus", &args);
        if !shown && sound {
            beep();
        }
        shown
    }

    pub fn beep() {
        if !spawn("canberra-gtk-play", &["-i", "dialog-warning"]) {
            spawn(
                "paplay",
                &["/usr/share/sounds/freedesktop/stereo/dialog-warning.oga"],
            );
        }
    }

    pub fn on_click(_f: Box<dyn Fn() + Send + Sync>) {}

    pub fn hide() {}
}

#[cfg(target_os = "macos")]
mod os {
    use super::spawn;

    /// A string in AppleScript's form.
    fn quoted(s: &str) -> String {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }

    pub fn show(title: &str, body: &str, sound: bool) -> bool {
        let mut script = format!(
            "display notification {} with title {}",
            quoted(body),
            quoted(title)
        );
        if sound {
            script.push_str(" sound name \"Glass\"");
        }
        spawn("osascript", &["-e", script.as_str()])
    }

    pub fn beep() {
        spawn("afplay", &["/System/Library/Sounds/Glass.aiff"]);
    }

    pub fn on_click(_f: Box<dyn Fn() + Send + Sync>) {}

    pub fn hide() {}
}

#[cfg(windows)]
mod os {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::OnceLock;

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_LARGE_ICON,
        NIIF_NOSOUND, NIIF_USER, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
        NIN_BALLOONUSERCLICK, NIN_SELECT, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, LoadIconW, LoadImageW,
        RegisterClassW, TranslateMessage, HICON, HWND_MESSAGE, IDI_APPLICATION, IMAGE_ICON,
        LR_DEFAULTSIZE, MB_ICONEXCLAMATION, MSG, WM_APP, WNDCLASSW,
    };

    /// The message the icon's events come in.
    const EVENTS: u32 = WM_APP + 1;
    /// The window that gets the icon's events (0: it could not be made).
    static WINDOW: OnceLock<usize> = OnceLock::new();
    static ICON: AtomicUsize = AtomicUsize::new(0);
    static SHOWN: AtomicBool = AtomicBool::new(false);
    static CLICK: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

    fn wide(s: &str, out: &mut [u16]) {
        let mut n = 0;
        for u in s.encode_utf16() {
            if n + 1 >= out.len() {
                break;
            }
            out[n] = u;
            n += 1;
        }
        out[n] = 0;
    }

    unsafe extern "system" fn events(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        if msg == EVENTS {
            let event = (l as u32) & 0xFFFF;
            if event == NIN_BALLOONUSERCLICK || event == NIN_SELECT {
                if let Some(f) = CLICK.get() {
                    f();
                }
            }
            return 0;
        }
        DefWindowProcW(hwnd, msg, w, l)
    }

    fn data(hwnd: HWND) -> NOTIFYICONDATAW {
        // SAFETY: a plain C struct, all zeros is its empty value.
        let mut d: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        d.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        d.hWnd = hwnd;
        d.uID = 1;
        d
    }

    /// A message-only window, with a thread of its own running its messages, made on first use.
    fn window() -> Option<HWND> {
        let h = *WINDOW.get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::Builder::new()
                .name("notify-icon".into())
                .spawn(move || unsafe {
                    let inst = GetModuleHandleW(std::ptr::null());
                    let class: Vec<u16> = "BigTextAlerts\0".encode_utf16().collect();
                    let mut wc: WNDCLASSW = std::mem::zeroed();
                    wc.lpfnWndProc = Some(events);
                    wc.hInstance = inst;
                    wc.lpszClassName = class.as_ptr();
                    RegisterClassW(&wc);
                    let hwnd = CreateWindowExW(
                        0,
                        class.as_ptr(),
                        std::ptr::null(),
                        0,
                        0,
                        0,
                        0,
                        0,
                        HWND_MESSAGE,
                        std::ptr::null_mut(),
                        inst,
                        std::ptr::null(),
                    );
                    // The executable's icon (resource 1), or the system's.
                    let mut icon =
                        LoadImageW(inst, 1 as _, IMAGE_ICON, 0, 0, LR_DEFAULTSIZE) as HICON;
                    if icon.is_null() {
                        icon = LoadIconW(std::ptr::null_mut(), IDI_APPLICATION);
                    }
                    ICON.store(icon as usize, Ordering::Relaxed);
                    let _ = tx.send(hwnd as usize);
                    if hwnd.is_null() {
                        return;
                    }
                    let mut msg: MSG = std::mem::zeroed();
                    while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                })
                .ok()
                .and_then(|_| rx.recv().ok())
                .unwrap_or(0)
        });
        (h != 0).then_some(h as HWND)
    }

    /// Puts the icon in the notification area (once); `false` when the shell refused it.
    fn add(hwnd: HWND) -> bool {
        if SHOWN.swap(true, Ordering::Relaxed) {
            return true;
        }
        let mut d = data(hwnd);
        d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        d.uCallbackMessage = EVENTS;
        d.hIcon = ICON.load(Ordering::Relaxed) as HICON;
        wide("BigText", &mut d.szTip);
        // SAFETY: `d` is a valid NOTIFYICONDATAW for this window.
        unsafe {
            if Shell_NotifyIconW(NIM_ADD, &d) == 0 {
                SHOWN.store(false, Ordering::Relaxed);
                return false;
            }
            d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            Shell_NotifyIconW(NIM_SETVERSION, &d);
        }
        true
    }

    pub fn show(title: &str, body: &str, sound: bool) -> bool {
        let Some(hwnd) = window() else { return false };
        if !add(hwnd) {
            return false;
        }
        let mut d = data(hwnd);
        d.uFlags = NIF_INFO;
        wide(title, &mut d.szInfoTitle);
        wide(body, &mut d.szInfo);
        d.dwInfoFlags = NIIF_USER | NIIF_LARGE_ICON | if sound { 0 } else { NIIF_NOSOUND };
        d.hBalloonIcon = ICON.load(Ordering::Relaxed) as HICON;
        // SAFETY: as in `add`.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) != 0 }
    }

    pub fn beep() {
        // SAFETY: no pointers.
        unsafe {
            MessageBeep(MB_ICONEXCLAMATION);
        }
    }

    pub fn on_click(f: Box<dyn Fn() + Send + Sync>) {
        let _ = CLICK.set(f);
    }

    pub fn hide() {
        let Some(&h) = WINDOW.get() else { return };
        if h == 0 || !SHOWN.swap(false, Ordering::Relaxed) {
            return;
        }
        let d = data(h as HWND);
        // SAFETY: as in `add`.
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &d);
        }
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod os {
    pub fn show(_title: &str, _body: &str, _sound: bool) -> bool {
        false
    }
    pub fn beep() {}
    pub fn on_click(_f: Box<dyn Fn() + Send + Sync>) {}
    pub fn hide() {}
}
