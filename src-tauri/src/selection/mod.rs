use std::thread;
use std::time::{Duration, Instant};

use arboard::Clipboard;

/// Capture currently selected text by simulating Ctrl+C, then restore clipboard.
///
/// Important: global shortcuts often fire while Ctrl/Shift are still held
/// (e.g. Ctrl+Alt+A). We wait for modifiers to release first, then
/// send a real Ctrl+C via Win32 SendInput.
pub fn capture_selection(timeout_ms: u64) -> Result<String, String> {
    wait_for_modifiers_release(Duration::from_millis(800))?;

    // Brief settle so the foreground app regains a clean key state
    thread::sleep(Duration::from_millis(40));

    let mut clipboard = Clipboard::new().map_err(|e| e.to_string())?;
    let previous = clipboard.get_text().unwrap_or_default();

    let marker = format!("__argos_marker_{}", uuid::Uuid::new_v4());
    clipboard
        .set_text(marker.clone())
        .map_err(|e| e.to_string())?;
    thread::sleep(Duration::from_millis(40));

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

#[cfg(windows)]
fn wait_for_modifiers_release(max_wait: Duration) -> Result<(), String> {
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
                return Ok(());
            }
        }
        if start.elapsed() > max_wait {
            // Proceed anyway; SendInput may still work
            eprintln!("argos: modifier keys still down after wait; continuing");
            return Ok(());
        }
        thread::sleep(Duration::from_millis(15));
    }
}

#[cfg(not(windows))]
fn wait_for_modifiers_release(_max_wait: Duration) -> Result<(), String> {
    Ok(())
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
