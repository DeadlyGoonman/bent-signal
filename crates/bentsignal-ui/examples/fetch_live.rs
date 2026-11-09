//! Live check of the update fetch against the real GitHub API (network). Prints the parsed latest
//! release and whether it is newer than 0.0.1 and than this build.

#[cfg(windows)]
fn main() {
    use bentsignal_ui::update::{is_newer, running_version};
    let json = bentsignal_ui::update_win::fetch_latest_json();
    println!("raw bytes: {:?}", json.as_ref().map(String::len));
    let Some(r) = json.as_deref().and_then(bentsignal_ui::update::parse_release) else {
        println!("no release (network failure, draft or prerelease)");
        return;
    };
    println!("tag: {}\nurl: {}", r.tag, r.url);
    println!("notes[..200]: {}", r.body.chars().take(200).collect::<String>());
    println!("0.0.1 < latest: {}", is_newer(&r.tag, "0.0.1"));
    println!("latest newer than this build ({}): {}", running_version(), is_newer(&r.tag, &running_version()));
}

#[cfg(not(windows))]
fn main() {}
