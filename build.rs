//! Embeds the application icon and version information into the Windows executables, so
//! Explorer, the "Open with BigText" context-menu entry and the taskbar show the BigText icon.

fn main() {
    println!("cargo:rerun-if-changed=assets/bigtext.ico");
    // docs.rs builds documentation on Linux without a resource compiler.
    let docs_rs = std::env::var_os("DOCS_RS").is_some();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") && !docs_rs {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/bigtext.ico")
            .set("FileDescription", "BigText")
            .set("ProductName", "BigText")
            .set("CompanyName", "Erkin Pardayev")
            .set("LegalCopyright", "Copyright (c) 2026 Erkin Pardayev, MIT license");
        if let Err(e) = res.compile() {
            panic!("embedding Windows resources failed: {e}");
        }
    }
}
