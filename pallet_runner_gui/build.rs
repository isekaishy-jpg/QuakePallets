fn main() {
    #[cfg(windows)]
    {
        let mut resource = winres::WindowsResource::new();
        resource.set_icon("assets/pallet_runner_gui.ico");
        if let Err(err) = resource.compile() {
            eprintln!("failed to embed icon: {}", err);
        }
    }
}
