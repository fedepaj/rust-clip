fn main() {
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/icon.ico"); // DEVE essere un file .ico
        res.compile().unwrap();
    }
}