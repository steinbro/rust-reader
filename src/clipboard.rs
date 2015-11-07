use winapi;
use user32;
use clipboard_win;

use clipboard_win::{get_clipboard_string, set_clipboard};
use clipboard_win::wrapper::get_clipboard_seq_num;
use std::mem;
use std::thread;


pub fn send_key_event(vk: u16, flags: u32) {
    let mut input = winapi::INPUT {
        type_: winapi::INPUT_KEYBOARD,
        u: [0u32; 6],
    };
    unsafe {
        *input.ki_mut() = winapi::KEYBDINPUT {
            wVk: vk,
            wScan: 0,
            dwFlags: flags,
            time: 0,
            dwExtraInfo: 0,
        };
        let mut b = &mut input;
        user32::SendInput(1, b, mem::size_of::<winapi::INPUT>() as i32);
    }
}

pub fn send_ctrl_c() {
    use winapi::{VK_CONTROL, KEYEVENTF_KEYUP};
    println!("sending ctrl-c");
    send_key_event(VK_CONTROL as u16, 0);
    send_key_event(67, 0); //ascii for "c"
    send_key_event(67, KEYEVENTF_KEYUP); //ascii for "c"
    send_key_event(VK_CONTROL as u16, KEYEVENTF_KEYUP);
}

pub fn what_on_clipboard_seq_num(clip_num: u32, n: u32) -> bool {
    for i in 1..(n + 1) {
        if get_clipboard_seq_num().unwrap_or(clip_num) != clip_num {
            return true;
        }
        thread::sleep_ms(10 * i);
    }
    get_clipboard_seq_num().unwrap_or(clip_num) != clip_num
}

pub fn get_text() -> Result<String, clipboard_win::WindowsError> {
    println!("geting text");
    let old_clip = get_clipboard_string();
    let old_clip_num = get_clipboard_seq_num().unwrap_or_else(|| {
        panic!("Lacks sufficient rights to access clipboard(WINSTA_ACCESSCLIPBOARD)")
    });
    send_ctrl_c();
    if !what_on_clipboard_seq_num(old_clip_num, 15) {
        return Err(clipboard_win::WindowsError::new(0));
    }
    let new_clip = get_clipboard_string();
    if let Ok(clip) = old_clip {
        let _ = set_clipboard(&clip);
    }
    new_clip
}