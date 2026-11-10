//! The Blanket logo for window and taskbar icons. The single source is
//! `docs/assets/logo.png`; `build.rs` scales it at build time, so nothing is decoded at
//! startup.

const ICON_64: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon-64.rgba"));
const ICON_256: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon-256.rgba"));

/// RGBA pixels of the logo at the prebuilt size closest to `size` (64 or 256).
pub fn app_icon_rgba(size: u32) -> (Vec<u8>, u32) {
    if size > 64 { (ICON_256.to_vec(), 256) } else { (ICON_64.to_vec(), 64) }
}
