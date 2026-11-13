fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=assets/tradecraft.ico");
    #[cfg(windows)]
    winresource::WindowsResource::new()
        .set_icon("assets/tradecraft.ico")
        .compile()?;
    Ok(())
}
