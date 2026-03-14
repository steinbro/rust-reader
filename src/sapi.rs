use average::{Estimate, Variance};
use chrono;
use std::mem::size_of;
use std::mem::{zeroed, MaybeUninit};

use windows::core::{w, PCWSTR};
use windows::Media::SpeechSynthesis as WinRtSpeech;
use windows::Storage::Streams as WinRtStreams;
use windows::Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi,
    Media::Audio,
    Media::Speech,
    System::Com as syscom,
    System::LibraryLoader,
    System::Registry as Reg,
    System::Threading::INFINITE,
    UI::Shell,
    UI::WindowsAndMessaging as wm,
};

use std::cmp::{max, min};
use std::mem;
use std::ops::Range;
use std::ptr::null_mut;
use std::time::Instant;

use crate::on_screen_control::*;
use crate::window::*;

/// A voice token representing either a legacy SAPI5 voice or a WinRT-only voice.
#[derive(Clone)]
enum VoiceToken {
    Sapi(Speech::ISpObjectToken),
    WinRt(WinRtSpeech::VoiceInformation),
}

impl VoiceToken {
    fn name(&self) -> String {
        match self {
            VoiceToken::Sapi(t) => SpVoice::get_voice_name_sapi(t.clone()),
            VoiceToken::WinRt(v) => v
                .DisplayName()
                .map(|s| s.to_string_lossy())
                .unwrap_or_else(|_| "unknown".to_string()),
        }
    }
}

/// Converts a SAPI5 rate (-10..=10) to a WinRT speaking rate (0.5..=6.0).
/// Uses an exponential mapping so that rate 0 → 1.0x, rate 10 → ~4x, rate -10 → ~0.25x (clamped to 0.5).
fn rate_sapi_to_winrt(sapi_rate: i32) -> f64 {
    2.0f64.powf(sapi_rate as f64 / 5.0).clamp(0.5, 6.0)
}

/// Returns `true` when the registry key identified by `voice_id` exists on this machine.
///
/// `VoiceInformation::Id()` returns a path like
/// `HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech_OneCore\Voices\Tokens\MSTTS_V110_enCA_LindaM`
/// which is the same registry key used by SAPI5. Opening that key is the most
/// reliable way to determine whether the voice is already enumerable via SAPI5,
/// regardless of how either API formats its display name or token-ID string.
fn voice_id_in_registry(voice_id: &str) -> bool {
    let (hive, subkey) =
        if let Some(rest) = voice_id.strip_prefix("HKEY_LOCAL_MACHINE\\") {
            (Reg::HKEY_LOCAL_MACHINE, rest)
        } else if let Some(rest) = voice_id.strip_prefix("HKEY_CURRENT_USER\\") {
            (Reg::HKEY_CURRENT_USER, rest)
        } else {
            return false;
        };
    let subkey_w: Vec<u16> = subkey
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut hkey = Reg::HKEY::default(); // null handle; overwritten on success by RegOpenKeyExW
    unsafe {
        if Reg::RegOpenKeyExW(
            hive,
            PCWSTR(subkey_w.as_ptr()),
            None, // uloptions: reserved, must be None (0)
            Reg::KEY_READ,
            &mut hkey,
        )
        .is_ok()
        {
            let _ = Reg::RegCloseKey(hkey);
            return true;
        }
    }
    false
}

/// Parses a WAV file header and returns `(WAVEFORMATEX, data_offset)` where
/// `data_offset` is the byte index of the first audio sample.
fn parse_wav_format(data: &[u8]) -> Option<(Audio::WAVEFORMATEX, usize)> {
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12usize;
    let mut wfx: Option<Audio::WAVEFORMATEX> = None;
    let mut data_offset: Option<usize> = None;
    while pos + 8 <= data.len() {
        let tag = &data[pos..pos + 4];
        let chunk_size =
            u32::from_le_bytes(data[pos + 4..pos + 8].try_into().ok()?) as usize;
        let chunk_data = pos + 8;
        if tag == b"fmt " && chunk_size >= 16 && chunk_data + 16 <= data.len() {
            wfx = Some(Audio::WAVEFORMATEX {
                wFormatTag: u16::from_le_bytes(
                    data[chunk_data..chunk_data + 2].try_into().ok()?,
                ),
                nChannels: u16::from_le_bytes(
                    data[chunk_data + 2..chunk_data + 4].try_into().ok()?,
                ),
                nSamplesPerSec: u32::from_le_bytes(
                    data[chunk_data + 4..chunk_data + 8].try_into().ok()?,
                ),
                nAvgBytesPerSec: u32::from_le_bytes(
                    data[chunk_data + 8..chunk_data + 12].try_into().ok()?,
                ),
                nBlockAlign: u16::from_le_bytes(
                    data[chunk_data + 12..chunk_data + 14].try_into().ok()?,
                ),
                wBitsPerSample: u16::from_le_bytes(
                    data[chunk_data + 14..chunk_data + 16].try_into().ok()?,
                ),
                cbSize: 0,
            });
        } else if tag == b"data" {
            data_offset = Some(chunk_data);
            break;
        }
        pos = chunk_data + chunk_size;
        if chunk_size % 2 != 0 {
            pos += 1;
        }
    }
    wfx.zip(data_offset)
}

/// Manages audio playback of WinRT-synthesized speech via the Win32 waveOut API.
struct WinRtVoiceState {
    synth: WinRtSpeech::SpeechSynthesizer,
    hwo: Option<Audio::HWAVEOUT>,
    // These buffers must outlive the waveOut device handle:
    _wav_buffer: Option<Box<Vec<u8>>>,
    wav_hdr: Option<Box<Audio::WAVEHDR>>,
    paused: bool,
}

impl WinRtVoiceState {
    fn new(
        voice_info: &WinRtSpeech::VoiceInformation,
        rate: i32,
    ) -> windows::core::Result<Self> {
        let synth = WinRtSpeech::SpeechSynthesizer::new()?;
        synth.SetVoice(voice_info)?;
        synth
            .Options()
            .and_then(|o| o.SetSpeakingRate(rate_sapi_to_winrt(rate)))
            .ok();
        Ok(WinRtVoiceState {
            synth,
            hwo: None,
            _wav_buffer: None,
            wav_hdr: None,
            paused: false,
        })
    }

    fn set_rate(&self, rate: i32) {
        self.synth
            .Options()
            .and_then(|o| o.SetSpeakingRate(rate_sapi_to_winrt(rate)))
            .ok();
    }

    /// Stop any active waveOut playback and release resources.
    fn stop(&mut self) {
        if let Some(hwo) = self.hwo.take() {
            unsafe {
                if let Some(hdr) = self.wav_hdr.as_mut() {
                    Audio::waveOutReset(hwo);
                    Audio::waveOutUnprepareHeader(
                        hwo,
                        hdr.as_mut(),
                        size_of::<Audio::WAVEHDR>() as u32,
                    );
                }
                Audio::waveOutClose(hwo);
            }
        }
        self.wav_hdr = None;
        self._wav_buffer = None;
        self.paused = false;
    }

    /// Synthesize `text` with WinRT and start waveOut playback asynchronously.
    fn speak(&mut self, text: &windows::core::HSTRING) {
        self.stop();

        // Synthesize text → WAV stream (blocks until synthesis is complete).
        let stream =
            match self
                .synth
                .SynthesizeTextToStreamAsync(text)
                .and_then(|op| op.get())
            {
                Ok(s) => s,
                Err(e) => {
                    println!("WinRT TTS synthesis failed: {:?}", e);
                    return;
                }
            };

        // Read the stream into a byte buffer.
        let size = match stream.Size() {
            Ok(s) => s as usize,
            Err(_) => return,
        };
        let input_stream = match stream.GetInputStreamAt(0) {
            Ok(s) => s,
            Err(_) => return,
        };
        let reader = match WinRtStreams::DataReader::CreateDataReader(&input_stream) {
            Ok(r) => r,
            Err(_) => return,
        };
        if reader
            .LoadAsync(size as u32)
            .and_then(|op| op.get())
            .is_err()
        {
            return;
        }
        let mut wav_data = vec![0u8; size];
        if reader.ReadBytes(&mut wav_data).is_err() {
            return;
        }

        // Parse the WAV header to get the audio format and data offset.
        let (wfx, audio_start) = match parse_wav_format(&wav_data) {
            Some(x) => x,
            None => {
                println!("WinRT TTS: failed to parse WAV header");
                return;
            }
        };
        let audio_len = wav_data.len().saturating_sub(audio_start);
        if audio_len == 0 {
            return;
        }

        // Keep the WAV buffer in a Box so its address is stable.
        let mut buf = Box::new(wav_data);

        // Open the waveOut device.
        let mut hwo = Audio::HWAVEOUT::default();
        let err = unsafe {
            Audio::waveOutOpen(
                Some(&mut hwo as *mut _),
                Audio::WAVE_MAPPER,
                &wfx as *const _,
                Some(0),
                Some(0),
                Audio::CALLBACK_NULL,
            )
        };
        if err != 0 {
            println!("waveOutOpen failed: {}", err);
            return;
        }

        // Prepare and submit the audio buffer.
        let mut hdr = Box::new(Audio::WAVEHDR {
            lpData: windows::core::PSTR::from_raw(buf[audio_start..].as_mut_ptr()),
            dwBufferLength: audio_len as u32,
            ..Default::default()
        });
        let err = unsafe {
            Audio::waveOutPrepareHeader(hwo, hdr.as_mut(), size_of::<Audio::WAVEHDR>() as u32)
        };
        if err != 0 {
            unsafe { Audio::waveOutClose(hwo) };
            println!("waveOutPrepareHeader failed: {}", err);
            return;
        }
        let err = unsafe {
            Audio::waveOutWrite(hwo, hdr.as_mut(), size_of::<Audio::WAVEHDR>() as u32)
        };
        if err != 0 {
            unsafe {
                Audio::waveOutUnprepareHeader(
                    hwo,
                    hdr.as_mut(),
                    size_of::<Audio::WAVEHDR>() as u32,
                );
                Audio::waveOutClose(hwo);
            }
            println!("waveOutWrite failed: {}", err);
            return;
        }

        self.hwo = Some(hwo);
        self._wav_buffer = Some(buf);
        self.wav_hdr = Some(hdr);
        self.paused = false;
    }

    fn pause(&mut self) {
        // HWAVEOUT is Copy; this copies the handle without consuming self.hwo.
        if let Some(hwo) = self.hwo {
            unsafe { Audio::waveOutPause(hwo) };
            self.paused = true;
        }
    }

    fn resume(&mut self) {
        // HWAVEOUT is Copy; this copies the handle without consuming self.hwo.
        if let Some(hwo) = self.hwo {
            unsafe { Audio::waveOutRestart(hwo) };
            self.paused = false;
        }
    }

    /// Block until the current waveOut buffer finishes playing.
    fn wait(&self) {
        while let Some(hdr) = self.wav_hdr.as_ref() {
            if hdr.dwFlags & Audio::WHDR_DONE != 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

impl Drop for WinRtVoiceState {
    fn drop(&mut self) {
        self.stop();
    }
}

pub const WM_SAPI_EVENT: u32 = wm::WM_APP + 15;
pub const WM_APP_NOTIFICATION_ICON: u32 = wm::WM_APP + 16;

pub struct Com {}

impl Com {
    pub fn new() -> Com {
        println!("new for Com");
        let hr = unsafe { syscom::CoInitialize(Some(null_mut())) };
        if hr.is_err() {
            panic!("CoInitialize failed: {:?}", hr);
        }
        Com {}
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        unsafe { syscom::CoUninitialize() };
        println!("drop for Com");
    }
}

pub struct SpVoice {
    // https://msdn.microsoft.com/en-us/library/ms723602.aspx
    voice: Speech::ISpVoice,
    window: HWND,
    controls: Box<OnScreenControlWindow>,
    edit: HWND,
    rate: HWND,
    reload_settings: HWND,
    show_controls: HWND,
    nicon: Shell::NOTIFYICONDATAW,
    last_read: WideString,
    last_update: Option<(Instant, Range<usize>)>,
    us_per_utf16: [Variance; 21],
    /// Active WinRT voice state; `Some` when a WinRT-only voice is selected.
    winrt_state: Option<WinRtVoiceState>,
    /// Most recently set speech rate (-10..=10), kept for WinRT re-synthesis.
    sapi_rate: i32,
}

impl SpVoice {
    pub fn new<'c>(_con: &'c Com) -> Box<SpVoice> {
        println!("new for SpVoice");

        unsafe {
            let mut out = Box::new(SpVoice {
                voice: syscom::CoCreateInstance(&Speech::SpVoice, None, syscom::CLSCTX_ALL)
                    .expect("failed for SpVoice at CoCreateInstance"),
                window: HWND(null_mut()),
                controls: OnScreenControlWindow::new(),
                edit: HWND(null_mut()),
                rate: HWND(null_mut()),
                reload_settings: HWND(null_mut()),
                show_controls: HWND(null_mut()),
                nicon: zeroed(),
                last_read: WideString::new(),
                last_update: None,
                us_per_utf16: Default::default(),
                winrt_state: None,
                sapi_rate: 0,
            });

            let window_class_name = w!("SAPI_event_window_class_name");
            wm::RegisterClassW(&wm::WNDCLASSW {
                style: wm::WNDCLASS_STYLES(0),
                lpfnWndProc: Some(window_proc_generic::<SpVoice>),
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
                wm::WINDOW_EX_STYLE(0),
                window_class_name,
                PCWSTR(&mut 0u16),
                wm::WS_OVERLAPPEDWINDOW | wm::WS_CLIPSIBLINGS | wm::WS_CLIPCHILDREN,
                0,
                0,
                0,
                0,
                Some(wm::GetDesktopWindow()),
                None,
                None,
                Some(&mut *out as *mut _ as _),
            )
            .expect("CreateWindowExW failed");

            out.nicon.cbSize = size_of::<Shell::NOTIFYICONDATAW>() as u32;
            out.nicon.hWnd = out.window;
            out.nicon.uCallbackMessage = WM_APP_NOTIFICATION_ICON;
            out.nicon.uID = 1 as u32;
            out.nicon.uFlags |= Shell::NIF_ICON;
            out.nicon.hIcon = wm::LoadIconW(
                Some(HINSTANCE(
                    LibraryLoader::GetModuleHandleW(PCWSTR::null()).unwrap().0,
                )),
                PCWSTR::from_raw(1 as *const u16),
            )
            .expect("failed to load icon");
            out.nicon.uFlags |= Shell::NIF_MESSAGE;
            out.nicon.Anonymous.uVersion = Shell::NOTIFYICON_VERSION_4;
            let err = Shell::Shell_NotifyIconW(Shell::NIM_ADD, &mut out.nicon);
            if err == false {
                panic!("failed for Shell_NotifyIconW NIM_ADD");
            }

            let err = Shell::Shell_NotifyIconW(Shell::NIM_SETVERSION, &mut out.nicon);
            if err == false {
                panic!("failed for Shell_NotifyIconW ");
            }

            out.edit = create_edit_window(
                out.window,
                wm::WS_VSCROLL
                    | wm::WINDOW_STYLE(wm::ES_MULTILINE as u32 | wm::ES_AUTOVSCROLL as u32),
            );
            out.rate = create_static_window(out.window, None);
            out.reload_settings = create_button_window(out.window, w!("Show Settings"));
            out.show_controls = create_button_window(out.window, w!("Show Controls"));
            move_window(
                out.window,
                &RECT {
                    left: 0,
                    top: 0,
                    right: 400,
                    bottom: 400,
                },
            );
            out.set_notify_window_message();
            out.set_volume(100);
            out.set_alert_boundary(Speech::SPEI_PHONEME);
            out.set_interest(
                &[
                    Speech::SPEI_WORD_BOUNDARY,
                    Speech::SPEI_START_INPUT_STREAM,
                    Speech::SPEI_END_INPUT_STREAM,
                ],
                &[],
            );
            out
        }
    }

    #[allow(dead_code)]
    pub fn get_window_handle(&mut self) -> HWND {
        self.window
    }

    pub fn set_time_estimater(&mut self, t: [Variance; 21]) {
        self.us_per_utf16 = t;
    }

    pub fn get_time_estimater(&self) -> [Variance; 21] {
        self.us_per_utf16.clone()
    }

    pub fn toggle_window_visible(&self) -> bool {
        toggle_window_visible(self.window)
    }

    #[allow(dead_code)]
    pub fn get_status_word(&mut self) -> String {
        let status = self.get_status();
        self.last_read.get_slice(status.word_range())
    }

    #[allow(dead_code)]
    pub fn get_status_sent(&mut self) -> String {
        let status = self.get_status();
        self.last_read.get_slice(status.sent_range())
    }

    pub fn speak<T: Into<WideString>>(&mut self, string: T) {
        self.last_read = string.into();
        set_window_text(self.edit, &self.last_read);
        if let Some(winrt) = self.winrt_state.as_mut() {
            let text = windows::core::HSTRING::from(self.last_read.as_string());
            winrt.speak(&text);
        } else {
            unsafe {
                self.voice.Speak(
                    PCWSTR::from_raw(self.last_read.as_ptr()),
                    (Speech::SVSFlagsAsync.0
                        | Speech::SVSFPurgeBeforeSpeak.0
                        | Speech::SVSFIsNotXML.0)
                        .try_into()
                        .unwrap(),
                    None,
                )
            }
            .unwrap();
        }
        self.last_update = None;
    }

    pub fn wait(&mut self) {
        if let Some(winrt) = self.winrt_state.as_ref() {
            winrt.wait();
        } else {
            unsafe { self.voice.WaitUntilDone(INFINITE) }.unwrap();
        }
    }

    pub fn speak_wait<T: Into<WideString>>(&mut self, string: T) {
        self.speak(string);
        self.wait();
    }

    pub fn pause(&mut self) {
        if let Some(winrt) = self.winrt_state.as_mut() {
            winrt.pause();
        } else {
            unsafe { self.voice.Pause() }.unwrap();
        }
        self.last_update = None;
    }

    pub fn resume(&mut self) {
        if let Some(winrt) = self.winrt_state.as_mut() {
            winrt.resume();
        } else {
            unsafe { self.voice.Resume() }.unwrap();
        }
        self.last_update = None;
    }

    pub fn set_rate(&mut self, rate: i32) -> i32 {
        let rate = max(min(rate, 10), -10);
        self.sapi_rate = rate;
        if let Some(winrt) = self.winrt_state.as_ref() {
            winrt.set_rate(rate);
        }
        // Always update the SAPI5 voice rate so that switching back from a WinRT
        // voice to a SAPI5 voice reflects the correct rate immediately.
        unsafe { self.voice.SetRate(rate) }.unwrap();
        self.last_update = None;
        self.get_rate()
    }

    pub fn get_rate(&mut self) -> i32 {
        let mut rate = 0;
        unsafe { self.voice.GetRate(&mut rate) }.unwrap();
        set_window_text(self.rate, &format!("reading at rate: {}", rate).into());
        rate
    }

    pub fn change_rate(&mut self, delta: i32) -> i32 {
        let rate = self.get_rate() + delta;
        self.set_rate(rate)
    }

    fn get_voice_name_sapi(token: Speech::ISpObjectToken) -> String {
        unsafe {
            token
                .OpenKey(w!("Attributes"))
                .ok()
                .and_then(|k| k.GetStringValue(w!("name")).ok())
                .and_then(|s| s.to_string().ok())
                .unwrap_or("unknown".to_string())
        }
    }

    fn available_sapi_voices() -> Vec<Speech::ISpObjectToken> {
        let mut voices = vec![];
        // Registry keys where voice tokens may be found
        let voice_categories = [
            w!(r"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech\Voices"),
            w!(r"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech_OneCore\Voices"),
        ];
        for c in voice_categories.iter() {
            unsafe {
                let category: Speech::ISpObjectTokenCategory = syscom::CoCreateInstance(
                    &Speech::SpObjectTokenCategory,
                    None,
                    syscom::CLSCTX_ALL,
                )
                .expect("create voice category");
                if let Err(_) = category.SetId(*c, false) {
                    // If registry key is not found, just try the next one
                    continue;
                }

                let token_enum = category.EnumTokens(w!(""), w!("")).expect("get voice list");
                voices.extend(std::iter::from_fn(|| {
                    let mut token = MaybeUninit::uninit();
                    token_enum
                        .Next(1, token.as_mut_ptr(), None)
                        .expect("iterate voices");
                    token.assume_init()
                }));
            }
        }
        voices
    }

    /// Returns all available voices, combining SAPI5 registry voices with any
    /// WinRT voices (e.g. newer Microsoft Natural voices) not already represented.
    ///
    /// Deduplication uses the Win32 registry directly: `VoiceInformation::Id()`
    /// returns the voice's registry key path (e.g.
    /// `HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech_OneCore\Voices\Tokens\MSTTS_V110_enCA_LindaM`).
    /// If that key exists, the voice is already in the SAPI5 list and is skipped.
    /// This avoids all string-format differences between `ISpObjectToken::GetId()`
    /// and `VoiceInformation::Id()` that caused previous deduplication attempts to fail.
    fn available_voices() -> Vec<VoiceToken> {
        // Start with SAPI5 voices from the Windows registry.
        let sapi_tokens = Self::available_sapi_voices();
        let mut voices: Vec<VoiceToken> = sapi_tokens.into_iter().map(VoiceToken::Sapi).collect();

        // Append WinRT voices that are not already represented by a SAPI5 token.
        // These include newer natural voices like Jenny and Aria that may only be
        // accessible through the Windows.Media.SpeechSynthesis API.
        if let Ok(winrt_voices) = WinRtSpeech::SpeechSynthesizer::AllVoices() {
            for voice_info in &winrt_voices {
                // VoiceInformation::Id() returns the voice's registry key path.
                // If that key exists the voice is already in SAPI5; skip it.
                // If Id() fails we cannot tell, so we add it (safe default).
                match voice_info.Id() {
                    Ok(id) if voice_id_in_registry(&id.to_string_lossy()) => {}
                    _ => voices.push(VoiceToken::WinRt(voice_info)),
                }
            }
        }
        voices
    }

    pub fn available_voice_names() -> Vec<String> {
        Self::available_voices()
            .iter()
            .map(|t| t.name())
            .collect::<Vec<_>>()
    }

    fn set_voice_sapi(&mut self, token: Speech::ISpObjectToken) {
        unsafe { self.voice.SetVoice(&token).ok() };
    }

    pub fn set_voice_by_name(&mut self, voice_name: String) -> String {
        let voices = Self::available_voices();
        if let Some(token) = voices.iter().find(|t| voice_name == t.name()) {
            match token {
                VoiceToken::Sapi(t) => {
                    // Use legacy SAPI5 path; clear any WinRT state.
                    self.winrt_state = None;
                    self.set_voice_sapi(t.clone());
                }
                VoiceToken::WinRt(v) => {
                    // Create a WinRT synthesizer for this voice.
                    match WinRtVoiceState::new(v, self.sapi_rate) {
                        Ok(state) => {
                            self.winrt_state = Some(state);
                        }
                        Err(e) => {
                            println!("Failed to create WinRT voice state: {:?}", e);
                        }
                    }
                }
            }
        }
        // Return the name of the voice now in use.
        if let Some(winrt) = &self.winrt_state {
            winrt
                .synth
                .Voice()
                .and_then(|v| v.DisplayName())
                .map(|s| s.to_string_lossy())
                .unwrap_or_else(|_| "unknown".to_string())
        } else {
            match unsafe { self.voice.GetVoice().ok() } {
                Some(t) => Self::get_voice_name_sapi(t),
                None => "unknown".to_string(),
            }
        }
    }

    /// Returns `true` when speech is currently paused (works for both SAPI5 and WinRT voices).
    pub fn is_paused(&self) -> bool {
        if let Some(winrt) = &self.winrt_state {
            winrt.paused
        } else {
            self.get_status_running_state() == 2
        }
    }

    /// Returns `dwRunningState` from the SAPI5 voice status.
    /// On error, returns 0 (idle/not-speaking), which is a safe default for
    /// `is_paused()` — the caller will assume the voice is not paused.
    fn get_status_running_state(&self) -> u32 {
        let mut status: Speech::SPVOICESTATUS = unsafe { mem::zeroed() };
        unsafe { self.voice.GetStatus(&mut status, null_mut()) }.ok();
        status.dwRunningState
    }

    pub fn set_volume(&mut self, volume: u16) {
        unsafe { self.voice.SetVolume(min(volume, 100)) }.unwrap();
    }

    #[allow(dead_code)]
    pub fn get_volume(&mut self) -> u16 {
        let mut volume = 0;
        unsafe { self.voice.GetVolume(&mut volume) }.unwrap();
        volume
    }

    pub fn set_alert_boundary(&mut self, boundary: Speech::SPEVENTENUM) {
        unsafe { self.voice.SetAlertBoundary(boundary) }.unwrap();
    }

    #[allow(dead_code)]
    pub fn get_alert_boundary(&mut self) -> Speech::SPEVENTENUM {
        let mut boundary = Speech::SPEVENTENUM(0);
        unsafe { self.voice.GetAlertBoundary(&mut boundary) }.unwrap();
        boundary
    }

    pub fn get_status(&mut self) -> Speech::SPVOICESTATUS {
        let mut status: Speech::SPVOICESTATUS = unsafe { mem::zeroed() };
        unsafe { self.voice.GetStatus(&mut status, null_mut()) }.unwrap();
        status
    }

    fn set_notify_window_message(&mut self) {
        unsafe {
            self.voice
                .SetNotifyWindowMessage(self.window, WM_SAPI_EVENT, WPARAM(0), LPARAM(0))
        }
        .unwrap();
    }

    pub fn set_interest(&mut self, event: &[Speech::SPEVENTENUM], queued: &[Speech::SPEVENTENUM]) {
        let queued = queued
            .iter()
            .map(|&x| {
                (1 << x.0) | (1 << Speech::SPEI_RESERVED1.0) | (1 << Speech::SPEI_RESERVED2.0)
            })
            .fold(0u64, |acc, x| acc | x);
        let event = event
            .iter()
            .map(|&x| {
                (1 << x.0) | (1 << Speech::SPEI_RESERVED1.0) | (1 << Speech::SPEI_RESERVED2.0)
            })
            .fold(queued, |acc, x| acc | x);
        unsafe { self.voice.SetInterest(event, queued) }.unwrap();
    }
}

fn format_duration(d: chrono::Duration) -> String {
    let h = d.num_hours();
    let m = d.num_minutes() - d.num_hours() * 60;
    let s = d.num_seconds() - d.num_minutes() * 60;
    if d.num_hours() == 0 {
        format!("{}:{:0>#2}", m, s)
    } else {
        format!("{}:{:0>#2}:{:0>#2}", h, m, s)
    }
}

#[test]
fn test_format_duration() {
    let duration = chrono::Duration::hours(25);
    assert_eq!(format_duration(duration), "25:00:00");
    let duration = chrono::Duration::hours(1) + chrono::Duration::seconds(1);
    assert_eq!(format_duration(duration), "1:00:01");
    let duration = chrono::Duration::hours(1) - chrono::Duration::seconds(1);
    assert_eq!(format_duration(duration), "59:59");
    let duration = chrono::Duration::seconds(61);
    assert_eq!(format_duration(duration), "1:01");
    let duration = chrono::Duration::seconds(60);
    assert_eq!(format_duration(duration), "1:00");
    let duration = chrono::Duration::seconds(59);
    assert_eq!(format_duration(duration), "0:59");
    let duration = chrono::Duration::seconds(9);
    assert_eq!(format_duration(duration), "0:09");
    let duration = chrono::Duration::seconds(0);
    assert_eq!(format_duration(duration), "0:00");
}

#[test]
fn test_rate_sapi_to_winrt() {
    // Rate 0 should map to 1.0x normal speed.
    assert!((rate_sapi_to_winrt(0) - 1.0).abs() < 1e-9);
    // Rate 5 should map to 2.0x.
    assert!((rate_sapi_to_winrt(5) - 2.0).abs() < 1e-9);
    // Rate 10 should map to 4.0x.
    assert!((rate_sapi_to_winrt(10) - 4.0).abs() < 1e-9);
    // Rate -10 is clamped to 0.5x minimum.
    assert!((rate_sapi_to_winrt(-10) - 0.5).abs() < 1e-9);
    // Result is always within the documented WinRT range [0.5, 6.0].
    for r in -10..=10 {
        let winrt = rate_sapi_to_winrt(r);
        assert!(winrt >= 0.5 && winrt <= 6.0, "rate {} → {} out of range", r, winrt);
    }
}

#[test]
fn test_parse_wav_format_valid() {
    // Minimal WAV: RIFF header + fmt chunk (16 bytes PCM) + data chunk (4 bytes).
    let mut wav = Vec::new();
    let fmt_chunk_size: u32 = 16;
    let data_chunk_size: u32 = 4;
    let riff_size: u32 = 4 + 8 + fmt_chunk_size + 8 + data_chunk_size;
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    // fmt chunk
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&fmt_chunk_size.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());    // wFormatTag = PCM
    wav.extend_from_slice(&1u16.to_le_bytes());    // nChannels = 1
    wav.extend_from_slice(&16000u32.to_le_bytes()); // nSamplesPerSec
    wav.extend_from_slice(&32000u32.to_le_bytes()); // nAvgBytesPerSec
    wav.extend_from_slice(&2u16.to_le_bytes());    // nBlockAlign
    wav.extend_from_slice(&16u16.to_le_bytes());   // wBitsPerSample
    // data chunk
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_chunk_size.to_le_bytes());
    wav.extend_from_slice(&[0u8; 4]);

    let result = parse_wav_format(&wav);
    assert!(result.is_some(), "should parse valid WAV");
    let (wfx, offset) = result.unwrap();
    assert_eq!(wfx.wFormatTag, 1);
    assert_eq!(wfx.nChannels, 1);
    assert_eq!(wfx.nSamplesPerSec, 16000);
    assert_eq!(wfx.wBitsPerSample, 16);
    assert_eq!(offset, wav.len() - 4); // data starts 4 bytes before end
}

#[test]
fn test_parse_wav_format_invalid() {
    // Not a WAV file.
    assert!(parse_wav_format(b"NotAWAVFile").is_none());
    // Empty slice.
    assert!(parse_wav_format(&[]).is_none());
    // RIFF but wrong WAVE marker.
    let mut bad = vec![0u8; 12];
    bad[0..4].copy_from_slice(b"RIFF");
    bad[8..12].copy_from_slice(b"MP3 ");
    assert!(parse_wav_format(&bad).is_none());
}

impl Windowed for SpVoice {
    fn window_proc(&mut self, msg: u32, w_param: WPARAM, l_param: LPARAM) -> Option<LRESULT> {
        match msg {
            wm::WM_DESTROY | wm::WM_QUERYENDSESSION | wm::WM_ENDSESSION => close(),
            WM_SAPI_EVENT => {
                let status = self.get_status();
                // convert rate from range (-10, 10) to (0, 20)
                let rate_shifted = 10u32
                    .checked_add_signed(self.get_rate())
                    .expect("bad rate < -10") as usize;
                let word_range = status.word_range();
                if word_range.end == 0 {
                    // called before start of reading.
                    self.last_update = None;
                    return Some(LRESULT(0));
                }
                if status.dwRunningState == 3 {
                    // called before end of reading.
                    let window_title = "100.0% 0:00 rust_reader".into();
                    set_console_title(&window_title);
                    set_window_text(self.window, &window_title);
                    self.last_update = None;
                    return Some(LRESULT(0));
                }
                if let Some((ref old_time, ref old_word_range)) = self.last_update {
                    if old_word_range.start == word_range.start {
                        return Some(LRESULT(0));
                    }
                    let elapsed = chrono::Duration::from_std(old_time.elapsed())
                        .expect("bad time diffrence.")
                        .num_microseconds()
                        .expect("bad time diffrence.");
                    let new_rate =
                        (elapsed as f64) / ((word_range.start - old_word_range.start) as f64);
                    self.us_per_utf16[rate_shifted].add(new_rate);
                }
                self.last_update = Some((Instant::now(), word_range.clone()));
                let read_len = self.last_read.len();
                // Guard against stale SAPI events where word positions exceed the
                // current buffer length (e.g. after a voice switch or failed WinRT
                // synthesis), which would otherwise cause an arithmetic overflow.
                if word_range.end > read_len || word_range.start > read_len {
                    self.last_update = None;
                    return Some(LRESULT(0));
                }
                let len_left = (read_len - word_range.end) as f64;
                let ms_left = len_left * self.us_per_utf16[rate_shifted].mean()
                    + (len_left * self.us_per_utf16[rate_shifted].sample_variance()).sqrt();
                let window_title = format!(
                    "{:.1}% {} \"{}\" rust_reader",
                    100.0 * (word_range.start as f64) / (self.last_read.len() as f64),
                    format_duration(chrono::Duration::microseconds(ms_left as i64)),
                    self.last_read.get_slice(word_range.clone())
                )
                .into();
                set_console_title(&window_title);
                set_window_text(self.window, &window_title);
                set_edit_selection(self.edit, &word_range);
                set_edit_scroll_caret(self.edit);
                return Some(LRESULT(0));
            }
            wm::WM_SIZE => {
                let rect = get_client_rect(self.window);
                if (w_param.0 <= 2) && rect.right > 0 && rect.bottom > 0 {
                    let (up, down) = rect.inset(3).split_rows(25);
                    move_window(self.edit, &down.inset(3));
                    let (left, right) = up.split_columns(240);
                    let (left_button, right_button) = left.split_columns(120);
                    move_window(self.reload_settings, &left_button.inset(3));
                    move_window(self.show_controls, &right_button.inset(3));
                    unsafe {
                        Gdi::InvalidateRect(Some(self.rate), None, true);
                    }
                    move_window(self.rate, &right.inset(3));
                    return Some(LRESULT(0));
                }
            }
            wm::WM_GETMINMAXINFO => {
                let data = unsafe { &mut *(l_param.0 as *mut u32 as *mut wm::MINMAXINFO) };
                data.ptMinTrackSize.x = 300;
                data.ptMinTrackSize.y = 110;
                return Some(LRESULT(0));
            }
            wm::WM_COMMAND => {
                use crate::press_hotkey;
                use crate::Action;
                if ((w_param.0 >> 16) & 0xffff) as u32 == wm::BN_CLICKED {
                    if self.reload_settings.0 as isize == l_param.0 {
                        press_hotkey(Action::ShowSettings);
                        return Some(LRESULT(0));
                    } else if self.show_controls.0 as isize == l_param.0 {
                        self.controls.toggle_controls_visible();
                        return Some(LRESULT(0));
                    }
                }
            }
            WM_APP_NOTIFICATION_ICON => {
                if (l_param.0 & 0xffff) as u32 == wm::WM_LBUTTONUP {
                    self.toggle_window_visible();
                    return Some(LRESULT(0));
                }
            }
            _ => {}
        }
        None
    }
}

impl Drop for SpVoice {
    fn drop(&mut self) {
        unsafe { Shell::Shell_NotifyIconW(Shell::NIM_DELETE, &mut self.nicon) };
        println!("drop for SpVoice");
    }
}

pub trait StatusUtil {
    fn word_range(&self) -> Range<usize>;
    fn sent_range(&self) -> Range<usize>;
}

impl StatusUtil for Speech::SPVOICESTATUS {
    fn word_range(&self) -> Range<usize> {
        self.ulInputWordPos as usize..(self.ulInputWordPos + self.ulInputWordLen) as usize
    }
    fn sent_range(&self) -> Range<usize> {
        self.ulInputSentPos as usize..(self.ulInputSentPos + self.ulInputSentLen) as usize
    }
}
