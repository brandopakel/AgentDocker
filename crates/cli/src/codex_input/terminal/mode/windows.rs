//! A managed console supplies raw characters to the bounded editor. Keep Ctrl-C
//! and output behavior unchanged; the guard owns a duplicate input handle so
//! restoring its original mode does not depend on the caller's handle lifetime.
use std::io;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Console::{
    ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_VIRTUAL_TERMINAL_INPUT, GetConsoleMode,
    SetConsoleMode,
};

pub(super) struct ConsoleMode {
    handle: OwnedHandle,
    saved: u32,
}

impl ConsoleMode {
    pub(super) fn enter(handle: OwnedHandle) -> io::Result<Self> {
        let raw = handle.as_raw_handle() as HANDLE;
        let mut saved = 0;
        // SAFETY: the owned handle and initialized output live through each
        // call. A pipe or unreadable console fails before any mode changes.
        if unsafe { GetConsoleMode(raw, &mut saved) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let active =
            (saved & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT)) | ENABLE_VIRTUAL_TERMINAL_INPUT;
        // Preserve processed input (Ctrl-C) and every unrelated setting.
        // Arrow/function keys become sequences the bounded editor discards.
        if unsafe { SetConsoleMode(raw, active) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle, saved })
    }
}

impl Drop for ConsoleMode {
    fn drop(&mut self) {
        // SAFETY: this guard still owns the duplicate console handle. Restore
        // exactly the mode captured at entry, including virtual input state.
        unsafe { SetConsoleMode(self.handle.as_raw_handle() as HANDLE, self.saved) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::System::Console::ENABLE_PROCESSED_INPUT;
    use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;

    fn settings(handle: &OwnedHandle) -> u32 {
        let mut mode = 0;
        // SAFETY: live handle and output; a failure never supplies settings.
        assert_ne!(
            unsafe { GetConsoleMode(handle.as_raw_handle() as HANDLE, &mut mode) },
            0
        );
        mode
    }

    #[test]
    fn console_editor_owns_echo_and_restores_mode_after_original_handle_closes() {
        const CHILD: &str = "AGENTDOCKER_TEST_PRIVATE_CONSOLE";
        if std::env::var_os(CHILD).is_none() {
            // A separate console avoids mutating the test runner's console or
            // any other test's state. The child has no provider or daemon.
            let module = module_path!().split_once("::").unwrap().1;
            let name = format!(
                "{module}::console_editor_owns_echo_and_restores_mode_after_original_handle_closes"
            );
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &name, "--nocapture"])
                .env(CHILD, "1")
                .creation_flags(CREATE_NEW_CONSOLE)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            while child.try_wait().unwrap().is_none() {
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("private console regression child timed out");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            return;
        }
        let input: OwnedHandle = OpenOptions::new()
            .read(true)
            .write(true)
            .open("CONIN$")
            .unwrap()
            .into();
        let witness = input.try_clone().unwrap();
        let original = settings(&witness);
        let guard = ConsoleMode::enter(input.try_clone().unwrap()).unwrap();
        drop(input);
        let active = settings(&witness);
        assert_eq!(active & (ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT), 0);
        assert_ne!(active & ENABLE_VIRTUAL_TERMINAL_INPUT, 0);
        assert_eq!(
            active & ENABLE_PROCESSED_INPUT,
            original & ENABLE_PROCESSED_INPUT
        );
        assert_eq!(
            active & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_VIRTUAL_TERMINAL_INPUT),
            original & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_VIRTUAL_TERMINAL_INPUT)
        );
        drop(guard);
        assert_eq!(settings(&witness), original);
    }

    #[test]
    fn non_console_handle_refuses_instead_of_claiming_echo_control() {
        let handle: OwnedHandle = OpenOptions::new().read(true).open("NUL").unwrap().into();
        assert!(ConsoleMode::enter(handle).is_err());
    }
}
