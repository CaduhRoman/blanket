fn main() {
    println!("cargo:rerun-if-changed=resources/blanket.ico");
    println!("cargo:rerun-if-changed=resources/silk.jpg");
    println!("cargo:rerun-if-changed=resources/newtab.html");
    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set("FileDescription", "Blanket Browser")
            .set("ProductName", "Blanket")
            .set("OriginalFilename", "blanket.exe");
        if std::path::Path::new("resources/blanket.ico").exists() {
            resource.set_icon("resources/blanket.ico");
        }
        if let Err(error) = resource.compile() {
            println!("cargo:warning=could not embed Windows resources: {error}");
        }
    }
}
