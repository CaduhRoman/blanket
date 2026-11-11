//! Embeds the Windows resources and derives every application icon from one source
//! image: `docs/assets/logo.png`. Replace that file to rebrand the browser — the
//! executable icon (.ico), the window and taskbar icons and the README all follow.

use std::path::PathBuf;

use image::imageops::FilterType;
use image::RgbaImage;

const LOGO: &str = "../../docs/assets/logo.png";
/// Sizes rendered for the window and taskbar icons at run time (raw RGBA in OUT_DIR).
const RUNTIME_SIZES: [u32; 2] = [64, 256];
/// GStreamer/GLib libraries the executable imports (`dumpbin /dependents`).
const DELAY_LOADED: &[&str] = &[
    "glib-2.0-0.dll",
    "gobject-2.0-0.dll",
    "gio-2.0-0.dll",
    "gstreamer-1.0-0.dll",
    "gstbase-1.0-0.dll",
    "gstapp-1.0-0.dll",
    "gstplay-1.0-0.dll",
    "gstvideo-1.0-0.dll",
    "gstaudio-1.0-0.dll",
    "gstwebrtc-1.0-0.dll",
    "gstsdp-1.0-0.dll",
];

fn main() {
    println!("cargo:rerun-if-changed={LOGO}");
    println!("cargo:rerun-if-changed=resources/silk.jpg");
    println!("cargo:rerun-if-changed=resources/newtab.html");
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));

    let logo = image::open(LOGO)
        .unwrap_or_else(|error| panic!("cannot read the application logo {LOGO}: {error}"))
        .to_rgba8();
    let logo = trimmed(&logo);
    for size in RUNTIME_SIZES {
        let pixels = scaled(&logo, size);
        std::fs::write(out_dir.join(format!("icon-{size}.rgba")), pixels.as_raw()).expect("write icon");
    }

    // With audio/video, GStreamer and GLib are loaded on first use rather than by the
    // Windows loader, so `media_runtime` can first find them (next to the executable or in
    // a system GStreamer install) and explain clearly when they are missing.
    if std::env::var_os("CARGO_FEATURE_MEDIA").is_some() &&
        std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
    {
        for dll in DELAY_LOADED {
            println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
        }
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
    }

    #[cfg(windows)]
    copy_angle_next_to_executable(&out_dir);

    #[cfg(windows)]
    {
        let icon_path = out_dir.join("blanket.ico");
        let mut resource = winresource::WindowsResource::new();
        resource
            .set("FileDescription", "Blanket Browser")
            .set("ProductName", "Blanket")
            .set("OriginalFilename", "blanket.exe");
        // Explorer, the taskbar and Alt+Tab use the icon embedded in the executable.
        match std::fs::write(&icon_path, ico(&logo, &[16, 20, 24, 32, 40, 48, 64, 96, 128, 256])) {
            Ok(()) => {
                resource.set_icon(icon_path.to_str().expect("OUT_DIR is valid UTF-8"));
            },
            Err(error) => println!("cargo:warning=could not write the application icon: {error}"),
        }
        if let Err(error) = resource.compile() {
            println!("cargo:warning=could not embed Windows resources: {error}");
        }
    }
}

/// Servo's rendering loads ANGLE with `LoadLibrary("libEGL.dll")`, so it must sit next to
/// blanket.exe. The mozangle build leaves it in its own OUT_DIR; copy it into the profile
/// folder (target\<profile>) so the built executable runs when opened directly.
#[cfg(windows)]
fn copy_angle_next_to_executable(out_dir: &std::path::Path) {
    // OUT_DIR = target\<profile>\build\blanket-<hash>\out
    let Some(build_dir) = out_dir.parent().and_then(std::path::Path::parent) else {
        return;
    };
    let Some(profile_dir) = build_dir.parent() else {
        return;
    };
    let newest_angle = std::fs::read_dir(build_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("mozangle-"))
        .map(|entry| entry.path().join("out"))
        .filter(|dir| dir.join("libEGL.dll").is_file())
        .max_by_key(|dir| dir.join("libEGL.dll").metadata().and_then(|meta| meta.modified()).ok());
    match newest_angle {
        Some(angle) => {
            for dll in ["libEGL.dll", "libGLESv2.dll"] {
                if let Err(error) = std::fs::copy(angle.join(dll), profile_dir.join(dll)) {
                    println!("cargo:warning=could not copy {dll} next to the executable: {error}");
                }
            }
        },
        None => println!("cargo:warning=ANGLE (libEGL.dll) not built yet; rebuild to place it next to blanket.exe"),
    }
}

/// Cut away the transparent margin around the artwork (keeping it square and centered,
/// with a hairline of breathing room), so icons fill their slot in the taskbar and Start
/// menu instead of showing a small mark inside a large empty square.
fn trimmed(logo: &RgbaImage) -> RgbaImage {
    let (width, height) = logo.dimensions();
    let (mut left, mut top, mut right, mut bottom) = (width, height, 0, 0);
    for (x, y, pixel) in logo.enumerate_pixels() {
        if pixel[3] > 16 {
            left = left.min(x);
            top = top.min(y);
            right = right.max(x);
            bottom = bottom.max(y);
        }
    }
    if right <= left || bottom <= top {
        return logo.clone();
    }
    let content = (right - left + 1).max(bottom - top + 1);
    let side = content + content / 50; // ~2% padding
    let center_x = (left + right) / 2;
    let center_y = (top + bottom) / 2;
    let mut square = RgbaImage::new(side, side);
    for y in 0..side {
        for x in 0..side {
            let source_x = i64::from(center_x) - i64::from(side / 2) + i64::from(x);
            let source_y = i64::from(center_y) - i64::from(side / 2) + i64::from(y);
            if (0..i64::from(width)).contains(&source_x) && (0..i64::from(height)).contains(&source_y) {
                square.put_pixel(x, y, *logo.get_pixel(source_x as u32, source_y as u32));
            }
        }
    }
    square
}

fn scaled(logo: &RgbaImage, size: u32) -> RgbaImage {
    image::imageops::resize(logo, size, size, FilterType::Lanczos3)
}

/// An `.ico` file with one 32-bit BMP image per size.
#[cfg_attr(not(windows), allow(dead_code))]
fn ico(logo: &RgbaImage, sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&size| bmp_entry(&scaled(logo, size))).collect();
    let mut file = Vec::new();
    // ICONDIR: reserved, type 1 (icon), image count.
    file.extend_from_slice(&0u16.to_le_bytes());
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&size, image) in sizes.iter().zip(&images) {
        // ICONDIRENTRY: 0 means 256 for width and height.
        let side = if size >= 256 { 0 } else { size as u8 };
        file.extend_from_slice(&[side, side, 0, 0]);
        file.extend_from_slice(&1u16.to_le_bytes()); // color planes
        file.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        file.extend_from_slice(&(image.len() as u32).to_le_bytes());
        file.extend_from_slice(&offset.to_le_bytes());
        offset += image.len() as u32;
    }
    for image in images {
        file.extend_from_slice(&image);
    }
    file
}

/// BITMAPINFOHEADER + bottom-up BGRA pixels + an all-zero AND mask.
fn bmp_entry(image: &RgbaImage) -> Vec<u8> {
    let size = image.width();
    let rgba = image.as_raw();
    let mask_row = size.div_ceil(32) * 4;
    let mut data = Vec::new();
    data.extend_from_slice(&40u32.to_le_bytes());
    data.extend_from_slice(&(size as i32).to_le_bytes());
    // Height covers the color bitmap and the mask.
    data.extend_from_slice(&((size * 2) as i32).to_le_bytes());
    data.extend_from_slice(&1u16.to_le_bytes());
    data.extend_from_slice(&32u16.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    data.extend_from_slice(&(size * size * 4 + mask_row * size).to_le_bytes());
    data.extend_from_slice(&[0; 16]); // resolution and palette counts
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            data.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    data.extend(std::iter::repeat_n(0u8, (mask_row * size) as usize));
    data
}
