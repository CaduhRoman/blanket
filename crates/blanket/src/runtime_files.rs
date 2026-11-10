//! Checking, before anything starts, that the files the browser ships with are present.
//!
//! - ANGLE (`libEGL.dll`, `libGLESv2.dll`) must sit next to `blanket.exe`: Servo's
//!   rendering loads it by name, and a different `libEGL.dll` found elsewhere on the
//!   system would crash it.
//! - With audio/video, the GStreamer and GLib DLLs are delay-loaded (see `build.rs`).
//!   They are used from next to `blanket.exe` (the packaged browser) or, otherwise, from
//!   a system GStreamer install (`GSTREAMER_1_0_ROOT_MSVC_X86_64` or the installer's
//!   default folder), which is added to the DLL search path.
//!
//! When something is missing the user gets a clear message instead of a loader error or
//! a crash.

use std::path::Path;
#[cfg(feature = "media")]
use std::path::PathBuf;

use tracing::error;

/// Make the runtime files loadable. Returns `false` (after telling the user) when the
/// browser cannot start.
pub fn prepare() -> bool {
    let Some(exe_dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_owned)) else {
        return true;
    };
    let mut missing = Vec::new();
    for angle in ["libEGL.dll", "libGLESv2.dll"] {
        if !exe_dir.join(angle).is_file() {
            missing.push(angle.to_owned());
        }
    }
    #[cfg(feature = "media")]
    if !prepare_gstreamer(&exe_dir) {
        missing.push("gstreamer-1.0-0.dll".to_owned());
    }
    if missing.is_empty() {
        return true;
    }
    error!(?missing, folder = %exe_dir.display(), "files the browser ships with are missing");
    show_missing_files(&missing);
    false
}

#[cfg(feature = "media")]
fn prepare_gstreamer(exe_dir: &Path) -> bool {
    const CORE_DLL: &str = "gstreamer-1.0-0.dll";
    if exe_dir.join(CORE_DLL).is_file() {
        return true;
    }
    let from_env = std::env::var_os("GSTREAMER_1_0_ROOT_MSVC_X86_64").map(PathBuf::from);
    let default = std::env::var_os("ProgramFiles")
        .map(|folder| Path::new(&folder).join("gstreamer").join("1.0").join("msvc_x86_64"));
    let Some(bin) = [from_env, default]
        .into_iter()
        .flatten()
        .map(|root| root.join("bin"))
        .find(|bin| bin.join(CORE_DLL).is_file())
    else {
        return false;
    };
    tracing::info!(path = %bin.display(), "using the system GStreamer runtime");
    let wide = wide(bin.as_os_str());
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call.
    unsafe {
        let _ = windows::Win32::System::LibraryLoader::SetDllDirectoryW(windows::core::PCWSTR(wide.as_ptr()));
    }
    true
}

fn show_missing_files(missing: &[String]) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let text = format!(
        "O Blanket não encontrou arquivos que vêm junto com ele ({}).\n\n\
         Reinstale o Blanket pelo instalador ou pelo pacote completo, ou mantenha o blanket.exe \
         na mesma pasta dos arquivos .dll que vêm com ele.\n\n\
         Blanket could not find files it ships with. Reinstall it, or keep blanket.exe next to \
         the .dll files it comes with.",
        missing.join(", ")
    );
    let text = wide(std::ffi::OsStr::new(&text));
    let title = wide(std::ffi::OsStr::new("Blanket"));
    // SAFETY: both strings are NUL-terminated UTF-16 buffers alive for the call.
    unsafe {
        MessageBoxW(
            None,
            windows::core::PCWSTR(text.as_ptr()),
            windows::core::PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    text.encode_wide().chain(Some(0)).collect()
}
