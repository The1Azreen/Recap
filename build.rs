fn main() {
    // Give the .exe its icon and version info. Needs the Windows SDK's rc.exe; without it we
    // still build, just with the default icon.
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico")
            .set("ProductName", "Recap")
            .set("FileDescription", "Recap - transcripts and summaries");
        if let Err(e) = res.compile() {
            println!("cargo:warning=no icon embedded: {e}");
        }
    }
    println!("cargo:rerun-if-changed=assets/icon.ico");
}
