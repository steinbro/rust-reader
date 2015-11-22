extern crate winapi;
extern crate ole32;
extern crate user32;
extern crate clipboard_win;
extern crate rustc_serialize; //To write rust objects to json

use rustc_serialize::json;
use std::io::prelude::*;
use std::fs::File;

use std::ptr;
use std::mem;

mod sapi;
use sapi::*;

mod clipboard;
use clipboard::*;

mod hot_key;
use hot_key::*;

#[derive(RustcEncodable, RustcDecodable, Debug)]
struct Settings {
    rate: i32,
}

impl Settings {
    pub fn new() -> Settings {
        Settings { rate: 6 }
    }
    pub fn path() -> std::path::PathBuf {
        let mut path = std::env::current_exe().unwrap();
        path.set_extension("json");
        path
    }
    pub fn from_file() -> Settings {
        File::open(Settings::path())
            .map(|mut f| {
                let mut s = String::new();
                f.read_to_string(&mut s)
                 .map(|_| json::decode(&s).unwrap_or(Settings::new()))
                 .unwrap_or(Settings::new())
            })
            .unwrap_or(Settings::new())
    }
}

impl Drop for Settings {
    fn drop(&mut self) {
        json::encode(self)
            .map(|s| {
                File::create(Settings::path())
                    .map(|mut f| f.write_all(s.as_bytes()).unwrap_or(()))
                    .unwrap_or(())
            })
            .unwrap_or(());
    }
}

fn main() {
    let _com = Com::new();
    let mut voice = SpVoice::new();
    let mut settings = Settings::from_file();
    voice.set_volume(99);
    println!("volume :{:?}", voice.get_volume());
    voice.set_rate(settings.rate);
    println!("rate :{:?}", voice.get_rate());
    voice.set_alert_boundary(winapi::SPEI_PHONEME);
    println!("alert_boundary :{:?}", voice.get_alert_boundary());
    voice.speak_wait("Ready!");
    let _hk = [// TODO why do we nead to spesify the id.
               HotKey::new(2, 191, 0).unwrap(), // ctrl-? key
               HotKey::new(7, winapi::VK_ESCAPE as u32, 1).unwrap(), // ctrl-alt-shift-esk
               HotKey::new(7, 191, 2).unwrap(), // ctrl-alt-shift-?
               HotKey::new(2, winapi::VK_OEM_PERIOD as u32, 3).unwrap(), // ctrl-.
               HotKey::new(3, winapi::VK_OEM_MINUS as u32, 4).unwrap(), // ctrl-alt--
               HotKey::new(3, winapi::VK_OEM_PLUS as u32, 5).unwrap() /* ctrl-alt-= */];
    let mut msg: winapi::MSG = unsafe { mem::zeroed() };
    while unsafe { user32::GetMessageW(&mut msg, ptr::null_mut(), 0, 0) } > 0 {
        match msg.message {
            winapi::WM_HOTKEY => {
                match msg.wParam {
                    0 => {
                        voice.resume();
                        match get_text() {
                            Ok(x) => voice.speak(x),
                            Err(x) => {
                                voice.speak_wait("oops. error.");
                                println!("{:?}", x);
                            }
                        }
                    }
                    1 => {
                        break;
                    }
                    2 => {
                        println!("dwRunningState {}", voice.get_status().dwRunningState)
                    }
                    3 => {
                        match voice.get_status().dwRunningState {
                            2 => voice.pause(),
                            _ => voice.resume(),
                        }
                    }
                    4 => {
                        settings.rate = voice.get_rate() - 1;
                        voice.set_rate(settings.rate);
                        println!("rate :{:?}", settings.rate);
                    }
                    5 => {
                        settings.rate = voice.get_rate() + 1;
                        voice.set_rate(settings.rate);
                        println!("rate :{:?}", settings.rate);
                    }
                    _ => {
                        println!("unknown hot {}", msg.wParam);
                    }
                }
            }
            _ => {
                println!("{:?}", msg);
                unsafe {
                    user32::TranslateMessage(&msg);
                    user32::DispatchMessageW(&msg);
                }
            }
        }
    }
    voice.resume();
    voice.speak_wait("bye!");
}
