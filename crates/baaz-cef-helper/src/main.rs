//! A CEF subprocess: sandbox first, then the framework, then whatever process
//! type Chromium asked for. See `aui_cef::run_helper`.

fn main() {
    std::process::exit(aui_cef::run_helper());
}
