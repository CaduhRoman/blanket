// The logo rasterizer is shared with the browser; it only uses `std`.
#[allow(dead_code)]
mod app_icon {
    include!("src/app_icon.rs");
}

fn main() {
    println!("cargo:rerun-if-changed=src/app_icon.rs");
    println!("cargo:rerun-if-changed=resources/silk.jpg");
    println!("cargo:rerun-if-changed=resources/newtab.html");
    #[cfg(windows)]
    {
        let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
        let icon_path = out_dir.join("blanket.ico");
        let mut resource = winresource::WindowsResource::new();
        resource
            .set("FileDescription", "Blanket Browser")
            .set("ProductName", "Blanket")
            .set("OriginalFilename", "blanket.exe");
        // Explorer, the taskbar and Alt+Tab use the icon embedded in the executable.
        match std::fs::write(&icon_path, ico(&[16, 20, 24, 32, 40, 48, 64, 96, 128, 256])) {
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

/// An `.ico` file with one 32-bit BMP image per size.
#[cfg_attr(not(windows), allow(dead_code))]
fn ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&size| bmp_entry(size)).collect();
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
fn bmp_entry(size: u32) -> Vec<u8> {
    let (rgba, size) = app_icon::app_icon_rgba(size);
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
