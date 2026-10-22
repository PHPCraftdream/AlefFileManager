// SPDX-License-Identifier: MIT OR Apache-2.0
//! Autostart backends: an in-memory fake, file-based adapters (LaunchAgent
//! plist and XDG autostart) and, on Windows, the HKCU Run registry value.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{AutostartError, Backend, Format};

/// Fake backend for tests and logic checks; keeps the raw value verbatim.
#[derive(Debug, Default)]
pub struct MemoryBackend {
    slot: std::sync::Mutex<Option<String>>,
}

impl MemoryBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Backend for MemoryBackend {
    fn read(&self) -> Result<Option<String>, AutostartError> {
        let guard = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        Ok(guard.clone())
    }

    fn write(&self, raw: &str) -> Result<(), AutostartError> {
        let mut guard = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(raw.to_string());
        Ok(())
    }

    fn remove(&self) -> Result<(), AutostartError> {
        *self.slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(())
    }

    fn format(&self) -> Format {
        Format::Run
    }
}

/// XDG rule: an absolute `XDG_CONFIG_HOME` wins; empty or relative values are
/// ignored and `$HOME/.config` is used. `None` when neither is available.
pub fn resolve_config_home(config_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    if let Some(value) = config_home {
        if !value.is_empty() {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                return Some(path);
            }
        }
    }
    let home = home?;
    if home.is_empty() {
        return None;
    }
    Some(Path::new(home).join(".config"))
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn random_suffix() -> String {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        // Fallback: process-wide counter, still unique within this process.
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        return format!("{n:016x}");
    }
    let mut hex = String::with_capacity(16);
    for byte in bytes {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// File-backed record written atomically: temp file next to the target, then
/// rename. On unix the rename replaces the target atomically; elsewhere the
/// target is removed first, leaving a small window without the record.
#[derive(Debug, Clone)]
pub struct FileBackend {
    path: PathBuf,
}

impl FileBackend {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn temp_path(&self) -> PathBuf {
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let serial = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        self.path
            .with_file_name(format!(".{name}.tmp{}{serial:x}", random_suffix()))
    }

    fn write_atomic(&self, raw: &str) -> Result<(), AutostartError> {
        let temp = self.temp_path();
        let outcome = (|| -> io::Result<()> {
            // The adapter may point at a not-yet-existing directory.
            if let Some(parent) = self.path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            fs::write(&temp, raw)?;
            #[cfg(unix)]
            fs::rename(&temp, &self.path)?;
            #[cfg(not(unix))]
            {
                // Documented window: the record briefly does not exist.
                let _ = fs::remove_file(&self.path);
                fs::rename(&temp, &self.path)?;
            }
            Ok(())
        })();
        if let Err(e) = outcome {
            let _ = fs::remove_file(&temp);
            return Err(AutostartError::Io(e.to_string()));
        }
        Ok(())
    }
}

impl Backend for FileBackend {
    fn read(&self) -> Result<Option<String>, AutostartError> {
        match fs::read_to_string(&self.path) {
            Ok(raw) => Ok(Some(raw)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(AutostartError::Io(e.to_string())),
        }
    }

    fn write(&self, raw: &str) -> Result<(), AutostartError> {
        self.write_atomic(raw)
    }

    fn remove(&self) -> Result<(), AutostartError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AutostartError::Io(e.to_string())),
        }
    }

    fn format(&self) -> Format {
        Format::Desktop
    }
}

/// macOS user LaunchAgent plist backend rooted at a directory (normally
/// `~/Library/LaunchAgents`).
#[derive(Debug, Clone)]
pub struct LaunchAgent {
    inner: FileBackend,
}

impl LaunchAgent {
    pub fn new() -> Result<Self, AutostartError> {
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .ok_or_else(|| AutostartError::Platform("HOME is not set".to_string()))?;
        let root = PathBuf::from(home).join("Library").join("LaunchAgents");
        Ok(Self::with_root(&root))
    }

    pub fn with_root(root: &Path) -> Self {
        Self {
            inner: FileBackend::new(root.join(format!("{}.plist", super::LAUNCH_AGENT_LABEL))),
        }
    }

    pub fn path(&self) -> &Path {
        self.inner.path()
    }
}

impl Backend for LaunchAgent {
    fn read(&self) -> Result<Option<String>, AutostartError> {
        self.inner.read()
    }
    fn write(&self, raw: &str) -> Result<(), AutostartError> {
        self.inner.write(raw)
    }
    fn remove(&self) -> Result<(), AutostartError> {
        self.inner.remove()
    }
    fn format(&self) -> Format {
        Format::Plist
    }
}

/// Linux XDG autostart backend rooted at a config directory (normally
/// `$XDG_CONFIG_HOME` or `~/.config`); the record is
/// `<root>/autostart/alef-runtime-broker.desktop`.
#[derive(Debug, Clone)]
pub struct XdgAutostart {
    inner: FileBackend,
}

impl XdgAutostart {
    pub fn new() -> Result<Self, AutostartError> {
        let config_home = std::env::var("XDG_CONFIG_HOME").ok();
        let home = std::env::var_os("HOME").map(|h| h.to_string_lossy().into_owned());
        let root =
            resolve_config_home(config_home.as_deref(), home.as_deref()).ok_or_else(|| {
                AutostartError::Platform("neither XDG_CONFIG_HOME nor HOME is usable".to_string())
            })?;
        Ok(Self::with_root(&root))
    }

    pub fn with_root(root: &Path) -> Self {
        Self {
            inner: FileBackend::new(root.join("autostart").join(super::DESKTOP_FILE_NAME)),
        }
    }

    pub fn path(&self) -> &Path {
        self.inner.path()
    }
}

impl Backend for XdgAutostart {
    fn read(&self) -> Result<Option<String>, AutostartError> {
        self.inner.read()
    }
    fn write(&self, raw: &str) -> Result<(), AutostartError> {
        self.inner.write(raw)
    }
    fn remove(&self) -> Result<(), AutostartError> {
        self.inner.remove()
    }
    fn format(&self) -> Format {
        Format::Desktop
    }
}

/// The backend used by `Autostart::native()` on this platform.
#[derive(Debug)]
pub enum NativeBackend {
    #[cfg(windows)]
    Windows(WindowsRegistry),
    #[cfg(not(windows))]
    File(FileBackend),
}

impl Backend for NativeBackend {
    fn read(&self) -> Result<Option<String>, AutostartError> {
        match self {
            #[cfg(windows)]
            Self::Windows(b) => b.read(),
            #[cfg(not(windows))]
            Self::File(b) => b.read(),
        }
    }

    fn write(&self, raw: &str) -> Result<(), AutostartError> {
        match self {
            #[cfg(windows)]
            Self::Windows(b) => b.write(raw),
            #[cfg(not(windows))]
            Self::File(b) => b.write(raw),
        }
    }

    fn remove(&self) -> Result<(), AutostartError> {
        match self {
            #[cfg(windows)]
            Self::Windows(b) => b.remove(),
            #[cfg(not(windows))]
            Self::File(b) => b.remove(),
        }
    }

    fn format(&self) -> Format {
        match self {
            #[cfg(windows)]
            Self::Windows(b) => b.format(),
            #[cfg(not(windows))]
            Self::File(b) => b.format(),
        }
    }
}

/// HKCU registry Run value. Only on Windows; the FFI touches exactly one
/// value under the configured subkey.
#[cfg(windows)]
#[derive(Debug, Clone)]
pub struct WindowsRegistry {
    root: windows_sys::Win32::System::Registry::HKEY,
    subkey: String,
}

#[cfg(windows)]
impl WindowsRegistry {
    pub fn with_root(root: windows_sys::Win32::System::Registry::HKEY, subkey: &str) -> Self {
        Self {
            root,
            subkey: subkey.to_string(),
        }
    }

    pub fn new() -> Self {
        Self::with_root(
            windows_sys::Win32::System::Registry::HKEY_CURRENT_USER,
            "Software\\Microsoft\\Windows\\CurrentVersion\\Run",
        )
    }

    fn utf16(s: &str) -> Result<Vec<u16>, AutostartError> {
        if s.contains('\0') {
            return Err(AutostartError::Platform(
                "registry path contains NUL".to_string(),
            ));
        }
        Ok(s.encode_utf16().chain(std::iter::once(0)).collect())
    }
}

#[cfg(windows)]
impl Default for WindowsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(windows)]
impl Backend for WindowsRegistry {
    fn read(&self) -> Result<Option<String>, AutostartError> {
        use windows_sys::Win32::Foundation::{
            ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS,
        };
        use windows_sys::Win32::System::Registry::{
            RegCloseKey, RegGetValueW, RegOpenKeyExW, KEY_READ, RRF_RT_REG_SZ,
        };

        let subkey = Self::utf16(&self.subkey)?;
        let name = Self::utf16(super::RUN_VALUE_NAME)?;
        let mut key: windows_sys::Win32::System::Registry::HKEY = 0;
        // Invariant: `key` receives a valid open HKEY or the call failed.
        let status = unsafe { RegOpenKeyExW(self.root, subkey.as_ptr(), 0, KEY_READ, &mut key) };
        if status != ERROR_SUCCESS {
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            return Err(AutostartError::Platform(format!(
                "RegOpenKeyExW failed: {status}"
            )));
        }
        let result = (|| -> Result<Option<String>, AutostartError> {
            let mut size: u32 = 0;
            // Invariant: `key` is open and valid for the whole call.
            let status = unsafe {
                RegGetValueW(
                    key,
                    std::ptr::null(),
                    name.as_ptr(),
                    RRF_RT_REG_SZ,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut size,
                )
            };
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            if status != ERROR_SUCCESS && status != ERROR_MORE_DATA {
                return Err(AutostartError::Platform(format!(
                    "RegGetValueW (size) failed: {status}"
                )));
            }
            let mut buf = vec![0u16; size as usize / 2 + 1];
            let mut len = (buf.len() * 2) as u32;
            // Invariant: `key` is open, `buf` outlives the call.
            let status = unsafe {
                RegGetValueW(
                    key,
                    std::ptr::null(),
                    name.as_ptr(),
                    RRF_RT_REG_SZ,
                    std::ptr::null_mut(),
                    buf.as_mut_ptr().cast(),
                    &mut len,
                )
            };
            if status != ERROR_SUCCESS {
                return Err(AutostartError::Platform(format!(
                    "RegGetValueW failed: {status}"
                )));
            }
            let count = (len as usize / 2).min(buf.len());
            let raw = &buf[..count];
            let trimmed = match raw.iter().position(|&c| c == 0) {
                Some(end) => &raw[..end],
                None => raw,
            };
            String::from_utf16(trimmed)
                .map(Some)
                .map_err(|_| AutostartError::Damaged)
        })();
        // Invariant: `key` is open; close it exactly once on every path.
        unsafe { RegCloseKey(key) };
        result
    }

    fn write(&self, raw: &str) -> Result<(), AutostartError> {
        use windows_sys::Win32::Foundation::ERROR_SUCCESS;
        use windows_sys::Win32::System::Registry::{
            RegCloseKey, RegCreateKeyExW, RegSetValueExW, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE,
            REG_SZ,
        };

        if raw.contains('\0') {
            return Err(AutostartError::Platform(
                "registry value contains NUL".to_string(),
            ));
        }
        let subkey = Self::utf16(&self.subkey)?;
        let name = Self::utf16(super::RUN_VALUE_NAME)?;
        let mut value: Vec<u16> = raw.encode_utf16().collect();
        value.push(0);
        let mut key: windows_sys::Win32::System::Registry::HKEY = 0;
        // Invariant: `key` receives a valid open HKEY or the call failed.
        let status = unsafe {
            RegCreateKeyExW(
                self.root,
                subkey.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(AutostartError::Platform(format!(
                "RegCreateKeyExW failed: {status}"
            )));
        }
        // Invariant: `key` is open; `value` outlives the call.
        let status = unsafe {
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * 2) as u32,
            )
        };
        unsafe { RegCloseKey(key) };
        if status != ERROR_SUCCESS {
            return Err(AutostartError::Platform(format!(
                "RegSetValueExW failed: {status}"
            )));
        }
        Ok(())
    }

    fn remove(&self) -> Result<(), AutostartError> {
        use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
        use windows_sys::Win32::System::Registry::{
            RegCloseKey, RegDeleteValueW, RegOpenKeyExW, KEY_SET_VALUE,
        };

        let subkey = Self::utf16(&self.subkey)?;
        let name = Self::utf16(super::RUN_VALUE_NAME)?;
        let mut key: windows_sys::Win32::System::Registry::HKEY = 0;
        // Invariant: `key` receives a valid open HKEY or the call failed.
        let status =
            unsafe { RegOpenKeyExW(self.root, subkey.as_ptr(), 0, KEY_SET_VALUE, &mut key) };
        if status != ERROR_SUCCESS {
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(());
            }
            return Err(AutostartError::Platform(format!(
                "RegOpenKeyExW failed: {status}"
            )));
        }
        // Invariant: `key` is open for the call below.
        let status = unsafe { RegDeleteValueW(key, name.as_ptr()) };
        unsafe { RegCloseKey(key) };
        if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(AutostartError::Platform(format!(
                "RegDeleteValueW failed: {status}"
            )))
        }
    }

    fn format(&self) -> Format {
        Format::Run
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;
    use crate::autostart::{Autostart, Entry, Parsed};
    use std::ffi::OsString;

    fn entry(dir: &Path, name: &str, args: &[&str]) -> Entry {
        // The exe must be absolute on every platform; anchor it in the
        // per-test tempdir so the same tests run on unix and Windows.
        Entry::new(dir.join(name), args.iter().map(OsString::from).collect())
    }

    #[test]
    fn launch_agent_enable_disable_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent = LaunchAgent::with_root(dir.path());
        let e = entry(dir.path(), "alef", &["--broker"]);
        Autostart::new(agent.clone()).enable(&e).expect("enable");
        let raw = std::fs::read_to_string(agent.path()).expect("file");
        assert_eq!(
            agent.format().parse(&raw).expect("parse"),
            Parsed::Entry(e.clone())
        );
        assert_eq!(
            Autostart::new(agent.clone()).status().expect("status"),
            crate::autostart::Status::Enabled(e)
        );
        Autostart::new(agent.clone()).disable().expect("disable");
        assert!(!agent.path().exists());
    }

    #[test]
    fn launch_agent_foreign_record_is_left_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent = LaunchAgent::with_root(dir.path());
        let foreign = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n\t<key>Label</key>\n\t<string>com.other</string>\n\
\t<key>ProgramArguments</key>\n\t<array>\n\t\t<string>/bin/other</string>\n\t</array>\n\
</dict>\n</plist>\n";
        std::fs::write(agent.path(), foreign).expect("write foreign");
        let before = std::fs::read(agent.path()).expect("read");
        assert!(matches!(
            Autostart::new(agent.clone()).enable(&entry(dir.path(), "a", &[])),
            Err(AutostartError::NotOwned)
        ));
        assert_eq!(std::fs::read(agent.path()).expect("read"), before);
        assert!(matches!(
            Autostart::new(agent.clone()).disable(),
            Err(AutostartError::NotOwned)
        ));
    }

    #[test]
    fn launch_agent_leaves_no_temp_files_and_keeps_neighbours() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("neighbour.plist"), "keep me").expect("write");
        let agent = LaunchAgent::with_root(dir.path());
        let e = entry(dir.path(), "alef", &[]);
        let auto = Autostart::new(agent);
        auto.enable(&e).expect("enable");
        auto.enable(&e)
            .expect("re-enable overwrites its own record");
        let entries: Vec<String> = std::fs::read_dir(dir.path())
            .expect("readdir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert!(entries.iter().any(|n| n == "neighbour.plist"));
        assert_eq!(
            entries
                .iter()
                .filter(|n| n.starts_with('.') && n.contains(".tmp"))
                .count(),
            0,
            "temp files must not survive enable: {entries:?}"
        );
    }

    #[test]
    fn launch_agent_root_naming() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent = LaunchAgent::with_root(dir.path());
        assert_eq!(
            agent.path(),
            dir.path().join("org.alef.runtime.broker.plist")
        );
    }

    #[test]
    fn xdg_enable_disable_round_trip_and_single_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let xdg = XdgAutostart::with_root(dir.path());
        assert_eq!(
            xdg.path(),
            dir.path()
                .join("autostart")
                .join("alef-runtime-broker.desktop")
        );
        let auto = Autostart::new(xdg.clone());
        assert_eq!(
            auto.status().expect("status"),
            crate::autostart::Status::Disabled
        );
        auto.enable(&entry(dir.path(), "alef", &["run"]))
            .expect("enable");
        assert!(xdg.path().is_file());
        auto.enable(&entry(dir.path(), "alef", &["run", "fast"]))
            .expect("update");
        let desktops: Vec<_> = std::fs::read_dir(dir.path().join("autostart"))
            .expect("readdir")
            .collect();
        assert_eq!(desktops.len(), 1, "no duplicate records");
        let raw = std::fs::read_to_string(xdg.path()).expect("read");
        assert!(raw.contains("run\" \"fast"));
        auto.disable().expect("disable");
        assert!(!xdg.path().exists());
    }

    #[test]
    fn xdg_damaged_record_blocks_enable_and_disable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let xdg = XdgAutostart::with_root(dir.path());
        std::fs::create_dir_all(xdg.path().parent().expect("parent")).expect("mkdir");
        std::fs::write(xdg.path(), "\u{0} garbage \u{1}").expect("write");
        assert_eq!(
            Autostart::new(xdg.clone()).status().expect("status"),
            crate::autostart::Status::Damaged
        );
        let result = Autostart::new(xdg.clone()).enable(&entry(dir.path(), "a", &[]));
        assert!(matches!(result, Err(AutostartError::Damaged)));
        assert!(matches!(
            Autostart::new(xdg.clone()).disable(),
            Err(AutostartError::Damaged)
        ));
    }

    #[test]
    fn resolve_config_home_follows_xdg_rules() {
        assert_eq!(
            resolve_config_home(Some("/abs/cfg"), Some("/home/u")),
            Some(PathBuf::from("/abs/cfg"))
        );
        assert_eq!(
            resolve_config_home(Some(""), Some("/home/u")),
            Some(PathBuf::from("/home/u/.config"))
        );
        assert_eq!(
            resolve_config_home(Some("relative/cfg"), Some("/home/u")),
            Some(PathBuf::from("/home/u/.config"))
        );
        assert_eq!(
            resolve_config_home(None, Some("/home/u")),
            Some(PathBuf::from("/home/u/.config"))
        );
        assert_eq!(resolve_config_home(None, None), None);
        assert_eq!(resolve_config_home(None, Some("")), None);
    }
}

#[cfg(windows)]
#[cfg(test)]
mod windows_tests {
    use super::*;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, HKEY_CURRENT_USER, KEY_SET_VALUE,
        REG_OPTION_NON_VOLATILE,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    struct Cleanup(String);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let subkey = wide(&self.0);
            // Invariant: HKEY_CURRENT_USER is a valid root for the call.
            let status = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, subkey.as_ptr()) };
            if status != ERROR_SUCCESS {
                panic!("RegDeleteKeyW({}) failed: {status}", self.0);
            }
        }
    }

    fn scratch_subkey() -> (String, Cleanup) {
        let mut bytes = [0u8; 4];
        getrandom::fill(&mut bytes).expect("getrandom");
        let suffix: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let subkey = format!("Software\\AlefBrokerTest\\{suffix}");
        (subkey.clone(), Cleanup(subkey))
    }

    #[test]
    fn registry_round_trip_in_scratch_key() {
        let (subkey, _cleanup) = scratch_subkey();
        let backend = WindowsRegistry::with_root(HKEY_CURRENT_USER, &subkey);

        // No key yet: reading is None, removing is a no-op.
        assert_eq!(backend.read().expect("read"), None);
        backend.remove().expect("idempotent remove");

        // Create the key, then exercise write/read/overwrite/remove.
        let subkey_w = wide(&subkey);
        let mut key: windows_sys::Win32::System::Registry::HKEY = 0;
        // Invariant: `key` receives a valid open HKEY or the call failed.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                subkey_w.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(status, ERROR_SUCCESS, "RegCreateKeyExW: {status}");
        // Invariant: `key` is open; close it once done.
        unsafe { RegCloseKey(key) };

        backend
            .write("/x/alef.exe --alef-runtime-autostart")
            .expect("write");
        assert_eq!(
            backend.read().expect("read").as_deref(),
            Some("/x/alef.exe --alef-runtime-autostart")
        );
        backend.write("C:\\alef.exe \"a b\"").expect("rewrite");
        assert_eq!(
            backend.read().expect("read").as_deref(),
            Some("C:\\alef.exe \"a b\"")
        );

        backend.remove().expect("remove");
        assert_eq!(backend.read().expect("read"), None);
    }
}
