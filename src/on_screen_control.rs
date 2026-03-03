use crate::press_hotkey;
use crate::window::*;
use crate::Action;
use std::ptr::null_mut;
use windows::core::{w, PCWSTR};
use windows::Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi,
    System::LibraryLoader,
    UI::WindowsAndMessaging as wm,
};

pub struct OnScreenControlWindow {
    window: HWND,
    read: HWND,
    pause: HWND,
}

impl OnScreenControlWindow {
    pub fn new() -> Box<OnScreenControlWindow> {
        let mut out = Box::new(OnScreenControlWindow {
            window: HWND(null_mut()),
            read: HWND(null_mut()),
            pause: HWND(null_mut()),
        });

        let window_class_name = w!("on_screen_control_window_class_name");
        unsafe {
            wm::RegisterClassW(&wm::WNDCLASSW {
                style: wm::WNDCLASS_STYLES(0),
                lpfnWndProc: Some(window_proc_generic::<OnScreenControlWindow>),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: HINSTANCE(null_mut()),
                hIcon: wm::LoadIconW(
                    Some(HINSTANCE(
                        LibraryLoader::GetModuleHandleW(PCWSTR::null()).unwrap().0,
                    )),
                    PCWSTR::from_raw(1 as *const u16),
                )
                .expect("failed to load icon"),
                hCursor: wm::LoadCursorW(None, wm::IDI_APPLICATION).expect("failed to load icon"),
                hbrBackground: Gdi::HBRUSH(16 as _),
                lpszMenuName: PCWSTR::null(),
                lpszClassName: window_class_name,
            });
            out.window = wm::CreateWindowExW(
                // WS_EX_NOACTIVATE makes window interactive but unfocusable,
                // like an on-screen keyboard
                wm::WS_EX_NOACTIVATE,
                window_class_name,
                w!(""),
                wm::WS_OVERLAPPED | wm::WS_SYSMENU,
                0,
                0,
                0,
                0,
                Some(wm::GetDesktopWindow()),
                None,
                None,
                Some(&mut *out as *mut _ as *mut _),
            )
            .expect("CreateWindowExW failed");
            // HWND_TOPMOST sets window to always be on top
            wm::SetWindowPos(
                out.window,
                Some(wm::HWND_TOPMOST),
                0,
                0,
                0,
                0,
                wm::SWP_NOMOVE | wm::SWP_NOSIZE,
            );
            out.read = create_button_window(out.window, w!("read"));
            out.pause = create_button_window(out.window, w!("pause/resume"));
        }
        set_window_text(out.window, &"reader controls".into());
        move_window(
            out.window,
            &RECT {
                left: 0,
                top: 80,
                right: 0,
                bottom: 0,
            },
        );
        out
    }

    pub fn toggle_controls_visible(&self) -> bool {
        toggle_window_visible(self.window)
    }
}

impl Windowed for OnScreenControlWindow {
    fn window_proc(&mut self, msg: u32, w_param: WPARAM, l_param: LPARAM) -> Option<LRESULT> {
        match msg {
            wm::WM_CLOSE => {
                show_window(self.window, wm::SW_HIDE);
                return Some(LRESULT(0));
            }
            wm::WM_SIZE => {
                let rect = get_client_rect(self.window).inset(3);
                if (w_param.0 <= 2) && rect.right > 0 && rect.bottom > 0 {
                    let rect = rect.split_rows(rect.bottom - 68);
                    let (l, r) = rect.1.split_columns(rect.1.right / 2);
                    move_window(self.read, &l);
                    move_window(self.pause, &r);
                    return Some(LRESULT(0));
                }
            }
            wm::WM_GETMINMAXINFO => {
                let data = unsafe { &mut *(l_param.0 as *mut wm::MINMAXINFO) };
                data.ptMinTrackSize.x = 240;
                data.ptMinTrackSize.y = 110;
                return Some(LRESULT(0));
            }
            wm::WM_COMMAND | wm::WM_HSCROLL => {
                let hiword = ((w_param.0 >> 16) & 0xffff) as u32;

                if hiword == wm::BN_CLICKED {
                    if l_param.0 == self.read.0 as isize {
                        press_hotkey(Action::Read);
                    }
                    if l_param.0 == self.pause.0 as isize {
                        press_hotkey(Action::PlayPause);
                    }
                }
            }
            _ => {}
        }
        None
    }
}
