use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use arboard::Clipboard;

/// How long Ctrl+Alt+C is ignored after a search starts copying the selection.
const SELECTION_COPY_GUARD_MS: u64 = 1000;
/// Extra ignore time measured from the synthetic Ctrl+C itself.
const SELECTION_COPY_SEND_TAIL_MS: u64 = 400;

static COPY_GUARD_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

/// Capture currently selected text by simulating Ctrl+C, then restore clipboard.
///
/// Important: global shortcuts often fire while Ctrl/Shift are still held
/// (e.g. Ctrl+Alt+A). We wait for modifiers to release first, then
/// send a real Ctrl+C via Win32 SendInput. If they are still held, we do not
/// send Ctrl+C: with Alt down that chord is Ctrl+Alt+C and would open chat.
pub fn capture_selection(timeout_ms: u64) -> Result<String, String> {
    push_chat_guard(SELECTION_COPY_GUARD_MS);
    let released = wait_for_modifiers_release(Duration::from_millis(800))?;
    if !should_copy_selection(released) {
        eprintln!("argos: modifier keys still down; not sending Ctrl+C");
        return Ok(String::new());
    }

    // Brief settle so the foreground app regains a clean key state
    thread::sleep(Duration::from_millis(40));

    let mut clipboard = Clipboard::new().map_err(|e| e.to_string())?;
    let previous = clipboard.get_text().unwrap_or_default();

    let marker = format!("__argos_marker_{}", uuid::Uuid::new_v4());
    clipboard
        .set_text(marker.clone())
        .map_err(|e| e.to_string())?;
    thread::sleep(Duration::from_millis(40));

    push_chat_guard(SELECTION_COPY_SEND_TAIL_MS);
    send_ctrl_c()?;

    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(300));
    let mut captured = String::new();
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
        match clipboard.get_text() {
            Ok(text) if text != marker && !text.is_empty() => {
                captured = text;
                break;
            }
            _ => {}
        }
    }

    // Restore previous clipboard (best effort). Small delay helps some apps.
    thread::sleep(Duration::from_millis(20));
    let _ = clipboard.set_text(previous);

    Ok(captured.trim().to_string())
}

/// True when Ctrl+Alt+C should be ignored because search is copying a selection.
pub fn chat_shortcut_blocked_by_selection_copy() -> bool {
    guard_blocks_chat(unix_ms(), COPY_GUARD_UNTIL_MS.load(Ordering::SeqCst))
}

fn push_chat_guard(extra_ms: u64) {
    let now = unix_ms();
    loop {
        let current = COPY_GUARD_UNTIL_MS.load(Ordering::SeqCst);
        let next = extend_guard_deadline(current, now, extra_ms);
        if COPY_GUARD_UNTIL_MS
            .compare_exchange(current, next, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            break;
        }
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Send the synthetic Ctrl+C only after Ctrl/Alt/Shift have been released.
pub(crate) fn should_copy_selection(modifiers_released: bool) -> bool {
    modifiers_released
}

pub(crate) fn guard_blocks_chat(now_ms: u64, until_ms: u64) -> bool {
    until_ms != 0 && now_ms < until_ms
}

pub(crate) fn extend_guard_deadline(current_until: u64, now_ms: u64, extra_ms: u64) -> u64 {
    current_until.max(now_ms.saturating_add(extra_ms))
}

#[cfg(windows)]
fn wait_for_modifiers_release(max_wait: Duration) -> Result<bool, String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_MENU, VK_RCONTROL,
        VK_RMENU, VK_RSHIFT, VK_SHIFT, VK_SPACE,
    };

    let keys = [
        VK_CONTROL, VK_LCONTROL, VK_RCONTROL, VK_SHIFT, VK_LSHIFT, VK_RSHIFT, VK_MENU,
        VK_LMENU, VK_RMENU, VK_SPACE,
    ];

    let start = Instant::now();
    loop {
        let any_down = keys
            .iter()
            .any(|vk| unsafe { GetAsyncKeyState(vk.0 as i32) as u16 } & 0x8000 != 0);
        if !any_down {
            // Require a short quiet period so we don't race the key-up
            thread::sleep(Duration::from_millis(30));
            let still_down = keys
                .iter()
                .any(|vk| unsafe { GetAsyncKeyState(vk.0 as i32) as u16 } & 0x8000 != 0);
            if !still_down {
                return Ok(true);
            }
        }
        if start.elapsed() > max_wait {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(15));
    }
}

#[cfg(not(windows))]
fn wait_for_modifiers_release(_max_wait: Duration) -> Result<bool, String> {
    Ok(true)
}

#[cfg(windows)]
fn send_ctrl_c() -> Result<(), String> {
    use std::mem::size_of;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY,
        VK_CONTROL, VK_C,
    };

    unsafe fn key_input(vk: VIRTUAL_KEY, up: bool) -> INPUT {
        let mut flags = Default::default();
        if up {
            flags = KEYEVENTF_KEYUP;
        }
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    unsafe {
        let inputs = [
            key_input(VK_CONTROL, false),
            key_input(VK_C, false),
            key_input(VK_C, true),
            key_input(VK_CONTROL, true),
        ];
        let sent = SendInput(&inputs, size_of::<INPUT>() as i32);
        if sent as usize != inputs.len() {
            return Err(format!(
                "SendInput failed: sent {sent}/{}",
                inputs.len()
            ));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn send_ctrl_c() -> Result<(), String> {
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
    enigo
        .key(Key::Control, Direction::Press)
        .map_err(|e| e.to_string())?;
    enigo
        .key(Key::Unicode('c'), Direction::Click)
        .map_err(|e| e.to_string())?;
    enigo
        .key(Key::Control, Direction::Release)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{extend_guard_deadline, guard_blocks_chat, should_copy_selection};

    #[test]
    fn selection_copy_waits_until_modifiers_are_released() {
        assert!(should_copy_selection(true));
        assert!(!should_copy_selection(false));
    }

    #[test]
    fn chat_shortcut_stays_blocked_through_the_copy_and_not_after() {
        assert!(!guard_blocks_chat(1_000, 0));
        assert!(guard_blocks_chat(1_000, 2_000));
        assert!(!guard_blocks_chat(2_000, 2_000));
        let until = extend_guard_deadline(0, 1_000, 1_000);
        assert_eq!(until, 2_000);
        assert_eq!(extend_guard_deadline(until, 1_500, 400), until);
        assert_eq!(extend_guard_deadline(until, 1_900, 400), 2_300);
    }
}
