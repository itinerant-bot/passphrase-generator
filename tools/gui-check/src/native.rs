//! Independent native checks using UI Observation and targeted UI Automation.
//! No screenshots, OCR, keyboard focus changes, or product testing arguments.
use std::{
    error::Error,
    io::Read,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use ui_observation_protocol::{Content, ElementObservation, Fraction, ObservationBody};
use ui_observation_runtime::{
    MemoryLimits, MemoryObserver, MemorySnapshot, ObservedWindowsProcess, WindowsObservedCommand,
};
use windows::{
    Win32::{
        Foundation::{BOOL, HGLOBAL, HWND, LPARAM, POINT},
        Graphics::{Dwm::DwmFlush, Gdi::ClientToScreen},
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
            },
            DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard},
            Memory::{GlobalLock, GlobalSize, GlobalUnlock},
        },
        UI::{
            Accessibility::{
                CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationInvokePattern,
                IUIAutomationRangeValuePattern, IUIAutomationSelectionItemPattern,
                IUIAutomationTogglePattern, IUIAutomationValuePattern, TreeScope_Descendants,
                UIA_InvokePatternId, UIA_RangeValuePatternId, UIA_SelectionItemPatternId,
                UIA_TogglePatternId, UIA_ValuePatternId,
            },
            HiDpi::{
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
                SetThreadDpiAwarenessContext,
            },
            WindowsAndMessaging::{
                EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
            },
        },
    },
    core::BSTR,
};

type Failure = Box<dyn Error>;
type Latest = Arc<Mutex<Option<Result<MemorySnapshot, String>>>>;
const TIMEOUT: Duration = Duration::from_secs(15);

struct Child(ObservedWindowsProcess);
impl Drop for Child {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
    }
}
struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: This guard is created only after COM initializes on this same thread.
        unsafe {
            CoUninitialize();
        }
    }
}

pub fn run() -> Result<(), Failure> {
    // SAFETY: DPI awareness changes only this observer thread's coordinate interpretation.
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let program = std::env::args_os()
        .nth(1)
        .ok_or("Provide the Windows GUI executable path.")?;
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes)?;
    let nonce: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let mut command = WindowsObservedCommand::new(program);
    command.env("RUST_BACKTRACE", "1").capture_stdio();
    let mut observed = command.spawn(&nonce)?;
    let diagnostics = [observed.take_stdout(), observed.take_stderr()].map(|pipe| {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(pipe) = pipe {
                let _ = pipe.take(65536).read_to_string(&mut text);
            }
            text
        })
    });
    let (process, mut stream) = observed.into_parts();
    let mut child = Child(process);
    let process_id = child.0.id();
    let latest: Latest = Arc::new(Mutex::new(None));
    let frames = latest.clone();
    let reader = std::thread::spawn(move || -> Result<MemorySnapshot, String> {
        let mut observer = MemoryObserver::new(&nonce, MemoryLimits::default())?;
        let result = (|| {
            while let Some(record) = stream.read().map_err(|error| error.to_string())? {
                observer.push(record)?;
                *frames.lock().unwrap() = Some(observer.snapshot());
            }
            observer.finish()
        })();
        if let Err(error) = &result {
            *frames.lock().unwrap() = Some(Err(error.clone()));
        }
        result
    });
    let mut snapshot = wait(&latest, |frame| element(frame, "result.entropy").is_some())?;
    let hwnd = find_window(process_id)?;
    // SAFETY: The HWND is checked against the spawned process before every native operation.
    unsafe {
        ensure(
            IsWindowVisible(hwnd).as_bool(),
            "Native window is not visible",
        )?;
        DwmFlush()?;
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
    }
    let _apartment = Apartment;
    // SAFETY: COM is initialized and the resulting interface stays on this thread.
    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)? };
    // UI Automation activation requests a new accessibility tree from egui.
    // Wait for its subsequent completed frame before retaining any action target.
    let activation_frame = snapshot.completed_frame;
    // SAFETY: COM is initialized; this queries only our child-owned window.
    unsafe {
        let root = automation.ElementFromHandle(hwnd)?;
        let condition = automation.CreateTrueCondition()?;
        let _ = root.FindAll(TreeScope_Descendants, &condition)?;
    }
    snapshot = wait(&latest, |frame| {
        frame.completed_frame > activation_frame
            && ready(frame)
            && element(frame, "settings.complexity.minimum").is_some()
            && element(frame, "settings.complexity.maximum").is_some()
    })?;
    let bounds = required(&snapshot, "settings.complexity.amount")?;
    println!(
        "startup_viewport={:?} scramble_slider={:?}",
        bounds.viewport_rect, bounds.layout_rect
    );
    check_layout(&snapshot)?;
    ensure(
        !required(&snapshot, "settings.word_limit.value")?
            .states
            .enabled,
        "Word editor should initially be disabled",
    )?;
    ensure(
        text_value(&snapshot, "settings.character_limit.value", "128"),
        "Default character limit is not 128",
    )?;
    let default_entropy = entropy(&snapshot)?;
    println!(
        "native_window_visible=true dwm_presentation_confirmed=true fully_visible_controls=22 password_plaintext_omitted=true"
    );

    let target = native_element(
        &automation,
        hwnd,
        process_id,
        &snapshot,
        "settings.words.enabled",
    )?;
    // SAFETY: The UI Automation element belongs to our child; this is a targeted control action.
    unsafe {
        target
            .GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)?
            .Toggle()?;
    }
    snapshot = wait(&latest, |frame| {
        selected(frame, "settings.words.enabled") && ready(frame)
    })?;
    ensure(
        entropy(&snapshot)? < default_entropy,
        "Word cap did not reduce accessible entropy",
    )?;
    ensure(
        required(&snapshot, "settings.word_limit.value")?
            .states
            .enabled,
        "Word editor did not enable",
    )?;

    set_text(
        &automation,
        hwnd,
        process_id,
        &snapshot,
        "settings.word_limit.value",
        "4",
    )?;
    snapshot = wait(&latest, |frame| {
        text_value(frame, "settings.word_limit.value", "4") && ready(frame)
    })?;
    let expected = four_words::Generator::new("e".parse()?, 128, "-")?
        .with_max_words(NonZeroUsize::new(4).unwrap())
        .prepare()?
        .entropy()
        .bits;
    ensure(
        (entropy(&snapshot)? - expected).abs() < 0.0051,
        "GUI entropy differs from the generator",
    )?;
    ensure(
        public_content(&snapshot, "result.classification") == Some("Insecure"),
        "Incorrect entropy classification",
    )?;

    set_text(
        &automation,
        hwnd,
        process_id,
        &snapshot,
        "settings.character_limit.value",
        "63",
    )?;
    snapshot = wait(&latest, |frame| {
        text_value(frame, "settings.character_limit.value", "63") && ready(frame)
    })?;
    let target = native_element(
        &automation,
        hwnd,
        process_id,
        &snapshot,
        "settings.character_limit.slider",
    )?;
    // SAFETY: This range control is resolved from observed geometry and matched to the child PID.
    unsafe {
        target
            .GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(UIA_RangeValuePatternId)?
            .SetValue(128.0)?;
    }
    snapshot = wait(&latest, |frame| {
        text_value(frame, "settings.character_limit.value", "128") && ready(frame)
    })?;
    ensure(
        public_content(&snapshot, "settings.character_limit.slider") == Some("128"),
        "Slider and editor disagree",
    )?;
    println!(
        "checkbox_enabled_editor=true editable_word_limit=4 editable_character_limit=63 slider_synced_editor=128 entropy_matches_generator=true"
    );

    invoke(&automation, hwnd, process_id, &snapshot, "action.copy")?;
    snapshot = wait(&latest, |frame| {
        public_content(frame, "action.copy") == Some("Copied!")
    })?;
    let first = clipboard(hwnd)?;
    check_password(&snapshot, &first, 128, Some(4), '-')?;
    let before = snapshot.completed_frame;
    invoke(&automation, hwnd, process_id, &snapshot, "action.generate")?;
    snapshot = wait(&latest, |frame| {
        frame.completed_frame > before
            && ready(frame)
            && public_content(frame, "action.copy") == Some("Copy password")
    })?;
    invoke(&automation, hwnd, process_id, &snapshot, "action.copy")?;
    snapshot = wait(&latest, |frame| {
        public_content(frame, "action.copy") == Some("Copied!")
    })?;
    let second = clipboard(hwnd)?;
    check_password(&snapshot, &second, 128, Some(4), '-')?;
    ensure(
        first != second,
        "New password did not replace the previous suggestion",
    )?;
    println!(
        "copy_password_verified=true actual_length_matches_display=true word_cap_respected=true new_password_changed=true"
    );

    let target = native_element(
        &automation,
        hwnd,
        process_id,
        &snapshot,
        "settings.words.enabled",
    )?;
    // SAFETY: Targeted toggle of the child-owned checkbox, without global keyboard input.
    unsafe {
        target
            .GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)?
            .Toggle()?;
    }
    snapshot = wait(&latest, |frame| {
        !selected(frame, "settings.words.enabled") && ready(frame)
    })?;
    ensure(
        (entropy(&snapshot)? - default_entropy).abs() < 0.0051,
        "Removing the cap did not restore entropy",
    )?;
    let target = native_element(
        &automation,
        hwnd,
        process_id,
        &snapshot,
        "settings.separator.space",
    )?;
    // SAFETY: Native radio selection is targeted to this child-owned control.
    unsafe {
        target
            .GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)?
            .Select()?;
    }
    snapshot = wait(&latest, |frame| {
        selected(frame, "settings.separator.space") && ready(frame)
    })?;
    ensure(
        (entropy(&snapshot)? - default_entropy).abs() < 0.0051,
        "Fixed separator added entropy",
    )?;
    invoke(&automation, hwnd, process_id, &snapshot, "action.copy")?;
    snapshot = wait(&latest, |frame| {
        public_content(frame, "action.copy") == Some("Copied!")
    })?;
    check_password(&snapshot, &clipboard(hwnd)?, 128, None, ' ')?;
    println!(
        "word_cap_can_be_disabled=true fixed_separator_adds_zero_entropy=true space_password_length_verified=true"
    );

    // SAFETY: WM_CLOSE is posted only to the top-level window owned by our child PID.
    unsafe {
        PostMessageW(hwnd, WM_CLOSE, None, None)?;
    }
    let deadline = Instant::now() + TIMEOUT;
    while child.0.try_wait()?.is_none() {
        ensure(Instant::now() < deadline, "Window did not close")?;
        std::thread::sleep(Duration::from_millis(20));
    }
    let status = child.0.wait()?;
    if !status.success() {
        for reader in diagnostics {
            eprintln!("{}", reader.join().unwrap_or_default());
        }
        return Err(format!("GUI exited with {status}").into());
    }
    let final_frame = reader.join().map_err(|_| "Observer reader panicked")??;
    ensure(
        final_frame.stream_complete,
        "Observation stream did not close cleanly",
    )?;
    println!(
        "clean_window_close=true clean_observation_stream=true records={} completed_frame={}",
        final_frame.records_observed,
        final_frame.completed_frame.unwrap_or(0)
    );
    Ok(())
}

fn ensure(condition: bool, message: &str) -> Result<(), Failure> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned().into())
    }
}

fn wait(
    latest: &Latest,
    predicate: impl Fn(&MemorySnapshot) -> bool,
) -> Result<MemorySnapshot, Failure> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(snapshot) = latest.lock().unwrap().as_ref() {
            match snapshot {
                Ok(snapshot) if predicate(snapshot) => return Ok(snapshot.clone()),
                Err(error) => return Err(error.clone().into()),
                _ => (),
            }
        }
        if Instant::now() >= deadline {
            if let Some(Ok(snapshot)) = latest.lock().unwrap().as_ref() {
                eprintln!(
                    "timeout_metrics: frame={:?} cap_selected={} entropy={:?} word_editor_enabled={:?}",
                    snapshot.completed_frame,
                    selected(snapshot, "settings.words.enabled"),
                    public_content(snapshot, "result.entropy"),
                    element(snapshot, "settings.word_limit.value")
                        .map(|element| element.states.enabled)
                );
            }
            return Err("Timed out waiting for measured UI state".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn element<'a>(snapshot: &'a MemorySnapshot, key: &str) -> Option<&'a ElementObservation> {
    match &snapshot.latest.get(key)?.record {
        ObservationBody::Element(element) => Some(element),
        _ => None,
    }
}
fn required<'a>(
    snapshot: &'a MemorySnapshot,
    key: &str,
) -> Result<&'a ElementObservation, Failure> {
    element(snapshot, key).ok_or_else(|| format!("Missing observed control {key}").into())
}
fn public_content<'a>(snapshot: &'a MemorySnapshot, key: &str) -> Option<&'a str> {
    element(snapshot, key)?.content.as_ref()?.public_value()
}
fn ready(snapshot: &MemorySnapshot) -> bool {
    element(snapshot, "result.entropy").is_some()
}
fn selected(snapshot: &MemorySnapshot, key: &str) -> bool {
    element(snapshot, key).is_some_and(|element| element.states.selected)
}
fn text_value(snapshot: &MemorySnapshot, key: &str, expected: &str) -> bool {
    element(snapshot, key).is_some_and(|element| {
        element.text_value == Some(Content::digest_only(expected.as_bytes()))
    })
}
fn entropy(snapshot: &MemorySnapshot) -> Result<f64, Failure> {
    Ok(public_content(snapshot, "result.entropy")
        .ok_or("Missing entropy")?
        .strip_suffix(" bits")
        .ok_or("Unexpected entropy label")?
        .parse()?)
}
fn check_layout(snapshot: &MemorySnapshot) -> Result<(), Failure> {
    ensure(
        snapshot
            .latest
            .contains_key("revision.ui.render.text.scan-complete"),
        "Missing completed rendering scan",
    )?;
    for key in [
        "settings.character_limit.slider",
        "settings.character_limit.value",
        "settings.words.enabled",
        "settings.word_limit.slider",
        "settings.word_limit.value",
        "settings.language.english",
        "settings.language.french",
        "settings.language.german",
        "settings.separator.hyphen",
        "settings.separator.space",
        "settings.complexity.uppercase",
        "settings.complexity.digit",
        "settings.complexity.special",
        "settings.complexity.amount",
        "settings.complexity.minimum",
        "settings.complexity.maximum",
        "result.entropy",
        "result.classification",
        "result.password",
        "result.length",
        "action.generate",
        "action.copy",
    ] {
        let control = required(snapshot, key)?;
        control.validate()?;
        ensure(
            control.visible_fraction == Fraction::ONE,
            &format!("Control clipped: {key}"),
        )?;
    }
    let top = required(snapshot, "layout.title")?
        .layout_rect
        .min_y
        .millionths();
    let bottom = [
        "settings.complexity.minimum",
        "settings.complexity.maximum",
        "action.generate",
        "action.copy",
    ]
    .map(|key| required(snapshot, key).map(|element| element.layout_rect.max_y.millionths()))
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?
    .into_iter()
    .max()
    .unwrap();
    let height = required(snapshot, "layout.title")?
        .viewport_rect
        .max_y
        .millionths();
    ensure(
        (height - bottom - top).abs() <= 1_000_000,
        &format!(
            "Opening margins differ: top {top}, bottom {}",
            height - bottom
        ),
    )?;
    let option_rows = [
        "settings.complexity.uppercase",
        "settings.complexity.digit",
        "settings.complexity.special",
    ]
    .map(|key| required(snapshot, key).map(|control| control.layout_rect.min_y));
    let [uppercase, numbers, symbols] = option_rows;
    let (uppercase, numbers, symbols) = (uppercase?, numbers?, symbols?);
    ensure(
        uppercase == numbers && numbers == symbols,
        "Website requirements wrap onto multiple lines",
    )?;
    ensure(
        matches!(
            required(snapshot, "result.password")?.content,
            Some(Content::PrivateOmitted { .. })
        ),
        "Password plaintext is not omitted",
    )?;
    Ok(())
}

struct WindowSearch {
    process_id: u32,
    hwnd: Option<HWND>,
}
unsafe extern "system" fn window_callback(hwnd: HWND, context: LPARAM) -> BOOL {
    // SAFETY: EnumWindows passes our live stack context for the duration of this synchronous call.
    let search = unsafe { &mut *(context.0 as *mut WindowSearch) };
    let mut process_id = 0;
    // SAFETY: The HWND comes from EnumWindows and the output pointer is writable stack storage.
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));
    }
    // SAFETY: The HWND comes directly from EnumWindows.
    if process_id == search.process_id && unsafe { IsWindowVisible(hwnd) }.as_bool() {
        search.hwnd = Some(hwnd);
        BOOL(0)
    } else {
        BOOL(1)
    }
}
fn find_window(process_id: u32) -> Result<HWND, Failure> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let mut search = WindowSearch {
            process_id,
            hwnd: None,
        };
        // SAFETY: The callback borrows this stack context only during EnumWindows.
        let _ = unsafe {
            EnumWindows(
                Some(window_callback),
                LPARAM(&mut search as *mut WindowSearch as isize),
            )
        };
        if let Some(hwnd) = search.hwnd {
            return Ok(hwnd);
        }
        ensure(
            Instant::now() < deadline,
            "No child-owned visible window appeared",
        )?;
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn native_element(
    automation: &IUIAutomation,
    hwnd: HWND,
    process_id: u32,
    snapshot: &MemorySnapshot,
    key: &str,
) -> Result<IUIAutomationElement, Failure> {
    let control = required(snapshot, key)?;
    ensure(
        control.visible_fraction == Fraction::ONE,
        "Target control is clipped",
    )?;
    let rect = control.visible_rect;
    let mut origin = POINT::default();
    // SAFETY: The HWND is owned by our child and origin is initialized writable storage.
    unsafe {
        ClientToScreen(hwnd, &mut origin).ok()?;
    }
    // SAFETY: The HWND is child-owned and live.
    let scale = f64::from(unsafe { GetDpiForWindow(hwnd) }) / 96.0;
    let point = POINT {
        x: origin.x
            + (((rect.min_x.millionths() + rect.max_x.millionths()) as f64 / 2e6) * scale).round()
                as i32,
        y: origin.y
            + (((rect.min_y.millionths() + rect.max_y.millionths()) as f64 / 2e6) * scale).round()
                as i32,
    };
    // SAFETY: UI Automation remains on its initialized COM thread; the returned element is PID-checked.
    let element = unsafe { automation.ElementFromPoint(point)? };
    // SAFETY: Access to this COM interface remains on the initialized thread.
    let native_process_id = unsafe { element.CurrentProcessId()? } as u32;
    ensure(
        native_process_id == process_id,
        "Native control belongs to another process",
    )?;
    // SAFETY: The native element belongs to the spawned child and COM is initialized.
    let name = unsafe { element.CurrentName()? }.to_string();
    if !name.is_empty() {
        ensure(
            Some(name.as_str()) == control.accessible_name.public_value(),
            &format!("Native target disagrees with observation: {key}: {name}"),
        )?;
    }
    Ok(element)
}
fn set_text(
    automation: &IUIAutomation,
    hwnd: HWND,
    process_id: u32,
    snapshot: &MemorySnapshot,
    key: &str,
    value: &str,
) -> Result<(), Failure> {
    let target = native_element(automation, hwnd, process_id, snapshot, key)?;
    // SAFETY: This is a targeted native editor action on a child-owned element.
    unsafe {
        target
            .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)?
            .SetValue(&BSTR::from(value))?;
    }
    Ok(())
}
fn invoke(
    automation: &IUIAutomation,
    hwnd: HWND,
    process_id: u32,
    snapshot: &MemorySnapshot,
    key: &str,
) -> Result<(), Failure> {
    let target = native_element(automation, hwnd, process_id, snapshot, key)?;
    // SAFETY: This invokes only the PID-checked child-owned control.
    unsafe {
        target
            .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)?
            .Invoke()?;
    }
    Ok(())
}

fn clipboard(hwnd: HWND) -> Result<String, Failure> {
    // SAFETY: Open/close are paired and the global memory is locked only while copying bounded UTF-16.
    unsafe {
        OpenClipboard(hwnd)?;
        let result = (|| {
            let handle = HGLOBAL(GetClipboardData(13)?.0);
            let size = GlobalSize(handle);
            ensure((2..=1024).contains(&size), "Unexpected clipboard size")?;
            let pointer = GlobalLock(handle).cast::<u16>();
            ensure(!pointer.is_null(), "Could not read copied password")?;
            let units = std::slice::from_raw_parts(pointer, size / 2);
            let length = units
                .iter()
                .position(|unit| *unit == 0)
                .ok_or("Copied text is unterminated")?;
            let value = String::from_utf16(&units[..length]);
            let _ = GlobalUnlock(handle);
            Ok::<_, Failure>(value?)
        })();
        let _ = CloseClipboard();
        result
    }
}
fn check_password(
    snapshot: &MemorySnapshot,
    password: &str,
    max_length: usize,
    max_words: Option<usize>,
    separator: char,
) -> Result<(), Failure> {
    ensure(
        password.len() <= max_length && !password.is_empty(),
        "Copied password exceeds the length cap",
    )?;
    let words: Vec<_> = password.split(separator).collect();
    ensure(
        max_words.is_none_or(|cap| words.len() <= cap),
        "Copied password exceeds the word cap",
    )?;
    ensure(
        words
            .iter()
            .all(|word| word.len() >= 4 && word.bytes().all(|byte| byte.is_ascii_lowercase())),
        "Copied password has invalid words",
    )?;
    ensure(
        public_content(snapshot, "result.length")
            == Some(format!("{} characters  ·  {} words", password.len(), words.len()).as_str()),
        "Actual password length differs from the displayed length",
    )?;
    Ok(())
}
