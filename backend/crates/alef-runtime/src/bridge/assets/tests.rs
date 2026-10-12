// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::{Bridge, BridgeOptions, Commands};
use std::{fs, sync::Arc};
pub(super) fn app(selected: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let app = tempfile::tempdir().unwrap();
    let root = app.path().join(selected);
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("index.html"), b"public").unwrap();
    fs::write(root.join("app.js"), b"script").unwrap();
    (app, root)
}
#[test]
fn the_application_boundary_is_never_served_as_the_public_root() {
    for selected in ["dist", "web/build"] {
        let (app, root) = app(selected);
        for private in [
            "alef.ktav",
            ".env",
            ".git/config",
            "node_modules/private.js",
        ] {
            let path = app.path().join(private);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"private").unwrap();
        }
        let source = Source::open(app.path(), &root).unwrap();
        assert_eq!(source.read("/").unwrap().0, b"public");
        assert_eq!(source.read("/app.js").unwrap().0, b"script");
        for private in [
            "/alef.ktav",
            "/.env",
            "/.git/config",
            "/node_modules/private.js",
            "/dist/app.js",
        ] {
            assert!(source.read(private).is_err(), "{private}");
        }
        drop(source);
        fs::remove_dir_all(&root).unwrap();
        assert!(Source::open(app.path(), &root).is_err());
        assert!(Source::open(app.path(), app.path()).is_err());
    }
}
#[test]
fn selected_roots_outside_the_application_are_rejected() {
    let (application, _root) = app("dist");
    let (outside, selected) = app("dist");
    assert!(selected.is_dir());
    assert!(!selected.starts_with(application.path()));
    assert_eq!(
        Source::open(application.path(), &selected)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    drop(outside);
}
#[cfg(windows)]
#[test]
fn an_application_root_junction_is_rejected() {
    let (real, _root) = app("dist");
    let parent = tempfile::tempdir().unwrap();
    let application = parent.path().join("application");
    link(real.path(), &application);
    assert!(super::windows::root(&application).is_err());
    fs::remove_dir(application).unwrap();
}
#[tokio::test]
async fn missing_disk_directory_reports_not_found() {
    let result = Bridge::new(Commands::new(), None, None).await;
    assert_eq!(result.err().unwrap().kind(), io::ErrorKind::NotFound);
}
#[test]
fn url_components_are_decoded_exactly_once() {
    let (app, root) = app("dist");
    fs::write(root.join("hello world.txt"), b"space").unwrap();
    fs::create_dir(root.join("%2e%2e")).unwrap();
    fs::write(root.join("%2e%2e/alef.ktav"), b"literal percent").unwrap();
    let source = Source::open(app.path(), &root).unwrap();
    assert_eq!(source.read("/hello%20world.txt").unwrap().0, b"space");
    assert_eq!(
        source.read("/%252e%252e/alef.ktav").unwrap().0,
        b"literal percent"
    );
    let url = servo::ServoUrl::parse("native://app/app.js?x=../alef.ktav#fragment").unwrap();
    assert_eq!(source.read(url.path()).unwrap().0, b"script");
}
#[test]
fn portable_names_and_paths_must_obey_character_and_length_limits() {
    let (app, root) = app("dist");
    let disk = Source::open(app.path(), &root).unwrap();
    let archive = Source::prepare(super::archive_tests::input("dist")).unwrap();
    for source in [&disk, &archive] {
        for invalid in [
            "/%2e%2e/alef.ktav",
            "/a%2fb",
            "/a%5cb",
            "/%00",
            "/%GG",
            "/%FF",
            "//app.js",
            "/a//b",
            "/a/",
            "/.",
            "/..",
            "/%7f",
            "/%C2%85",
        ] {
            assert_eq!(
                source.read(invalid).unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "{invalid}"
            );
        }
        for bad in [
            "app.js",
            "/APP.JS.",
            "/app.js%20",
            "/app.js:secret",
            "/NUL.txt",
            "/COM1.js",
            "/COM%C2%B9.txt",
            "/LPT%C2%B2",
            "/LPT%C2%B3.js",
            "/con",
            "/a%01",
            "/a%3fb",
            "/a%22b",
            "/a%3cb",
            "/a%3eb",
            "/a%7cb",
            "/a%",
            "/a%0",
            "/a%2Gb",
            "/%C0%AF",
            "/%ED%A0%80",
            "/%F4%90%80%80",
            "/a*b",
            r"/a\b",
            "/PRN",
            "/AUX.log",
            "/COM9",
            "/LPT9.bin",
        ] {
            assert_eq!(
                source.read(bad).unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "{bad}"
            );
        }
        for (length, kind) in [
            (255, io::ErrorKind::NotFound),
            (256, io::ErrorKind::InvalidInput),
        ] {
            assert_eq!(
                source
                    .read(&format!("/{}", "a".repeat(length)))
                    .unwrap_err()
                    .kind(),
                kind
            );
        }
        // The missing first component avoids host filesystem total-path limits. An archive
        // cannot represent a 1024-byte relative name plus its root, but must still validate it.
        let decoded = format!(
            "/{}/{}/{}/{}/{}",
            "a".repeat(255),
            "b".repeat(255),
            "c".repeat(255),
            "d".repeat(254),
            "e"
        );
        assert_eq!(decoded.len() - 1, 1024);
        assert_eq!(
            source.read(&decoded).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            source.read(&(decoded + "e")).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        // Encoding every letter reaches 3065 raw bytes at the decoded ceiling.
        // 3072 raw bytes cannot pass the component and decoded limits simultaneously.
        let raw = format!(
            "/{}/{}/{}/{}/{}",
            "%61".repeat(255),
            "%62".repeat(255),
            "%63".repeat(255),
            "%64".repeat(254),
            "%65"
        );
        assert_eq!(raw.len(), 3065);
        assert_eq!(
            source.read(&raw).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let at_raw_ceiling = raw.clone() + "%65%65e";
        assert_eq!(at_raw_ceiling.len(), 3072);
        assert_eq!(
            source.read(&at_raw_ceiling).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let over = at_raw_ceiling + "e";
        assert_eq!(over.len(), 3073);
        assert_eq!(
            source.read(&over).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}

#[test]
fn assets_require_exact_long_names_and_matching_case() {
    let (app, root) = app("dist");
    fs::write(root.join("long asset filename.txt"), b"long").unwrap();
    let source = Source::open(app.path(), &root).unwrap();
    assert!(source.read("/APP.JS").is_err());
    assert!(source.read("/LONGAS~1.TXT").is_err());
    assert_eq!(
        source.read("/long%20asset%20filename.txt").unwrap().0,
        b"long"
    );
}
#[cfg(windows)]
#[test]
fn an_os_reported_short_alias_cannot_select_a_long_name() {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetShortPathNameW(long: *const u16, short: *mut u16, length: u32) -> u32;
    }
    let (app, root) = app("dist");
    let path = root.join("a genuinely long filename.txt");
    fs::write(&path, b"long").unwrap();
    let long: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut short = [0u16; 32768];
    // SAFETY: terminated input and writable output with supplied capacity; no ownership transfer.
    let length = unsafe { GetShortPathNameW(long.as_ptr(), short.as_mut_ptr(), short.len() as u32) }
        as usize;
    assert!(length > 0 && length < short.len());
    let short = String::from_utf16(&short[..length]).unwrap();
    let alias = short.rsplit('\\').next().unwrap();
    if alias == "a genuinely long filename.txt" {
        eprintln!("8.3 names disabled on test volume; no actual short alias available");
    } else {
        assert!(Source::open(app.path(), &root)
            .unwrap()
            .read(&format!("/{alias}"))
            .is_err());
        // The listing is bypassed: the name of the opened object alone must refuse the alias.
        let directory = super::windows::root(&root).unwrap();
        assert!(super::windows::child(&directory, alias, false).is_err());
    }
}
#[cfg(unix)]
#[test]
fn a_case_collision_is_rejected() {
    let (app, root) = app("dist");
    fs::write(root.join("APP.JS"), b"collision").unwrap();
    use std::os::unix::fs::MetadataExt;
    if fs::metadata(root.join("APP.JS")).unwrap().ino()
        == fs::metadata(root.join("app.js")).unwrap().ino()
    {
        eprintln!("case-sensitive collision fixture unavailable on this volume");
        return;
    }
    assert!(Source::open(app.path(), &root).is_err());
}
#[test]
fn only_siblings_that_fold_to_the_requested_name_matter() {
    let stored: Vec<String> = [".env", ".ENV", "dist", "app.js", "APP.JS", "Logo.png"]
        .map(String::from)
        .into();
    assert!(select(&stored, "dist").is_ok());
    assert!(select(&stored, "Logo.png").is_ok());
    assert_eq!(
        select(&stored, "app.js").unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        select(&stored, ".env").unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    for wrong in ["DIST", "logo.png", "missing"] {
        assert_eq!(
            select(&stored, wrong).unwrap_err().kind(),
            io::ErrorKind::NotFound,
            "{wrong}"
        );
    }
}
#[test]
fn the_control_root_of_a_package_cannot_be_the_public_root() {
    for selected in ["META-INF", "meta-inf"] {
        let (app, root) = app(selected);
        assert!(Source::open(app.path(), &root).is_err(), "{selected}");
    }
    let (app, root) = app("web/META-INF");
    assert!(Source::open(app.path(), &root).is_ok());
}
#[test]
fn a_file_over_the_ceiling_is_refused_however_it_grew() {
    let mut exact: &[u8] = b"12345";
    assert_eq!(read_bounded(&mut exact, 5).unwrap(), b"12345");
    let mut over: &[u8] = b"123456";
    assert_eq!(
        read_bounded(&mut over, 5).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}
#[test]
fn two_entries_that_fold_to_one_name_are_a_collision() {
    let names = |list: &[&str]| list.iter().map(|name| name.to_string()).collect::<Vec<_>>();
    assert!(reject_collisions(&names(&["a.js", "b.js", "Logo.png"])).is_ok());
    for clash in [&["a.js", "A.JS"][..], &["x", "y", "Straße", "STRAßE"][..]] {
        assert_eq!(
            reject_collisions(&names(clash)).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied,
            "{clash:?}"
        );
    }
}
#[cfg(unix)]
#[test]
fn collisions_among_private_siblings_do_not_stop_the_start() {
    use std::os::unix::fs::MetadataExt;
    let (app, root) = app("dist");
    fs::write(app.path().join(".env"), b"one").unwrap();
    fs::write(app.path().join(".ENV"), b"two").unwrap();
    if fs::metadata(app.path().join(".env")).unwrap().ino()
        == fs::metadata(app.path().join(".ENV")).unwrap().ino()
    {
        eprintln!("case-sensitive collision fixture unavailable on this volume");
        return;
    }
    let source = Source::open(app.path(), &root).unwrap();
    assert_eq!(source.read("/app.js").unwrap().0, b"script");
}
#[cfg(windows)]
#[test]
fn a_file_with_a_reparse_tag_that_is_no_link_is_not_served() {
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn DeviceIoControl(
            handle: *mut std::ffi::c_void,
            code: u32,
            input: *const std::ffi::c_void,
            input_length: u32,
            output: *mut std::ffi::c_void,
            output_length: u32,
            returned: *mut u32,
            overlapped: *mut std::ffi::c_void,
        ) -> i32;
    }
    // A Microsoft-range tag that is no name surrogate: std still reports a regular file.
    fn mark(path: &Path) {
        let mut buffer = Vec::from(0x8000_0FF0u32.to_le_bytes());
        buffer.extend([4, 0, 0, 0, 1, 2, 3, 4]);
        let file = fs::OpenOptions::new().write(true).open(path).unwrap();
        let mut returned = 0;
        // SAFETY: live handle; the input pointer and length describe one buffer; no output buffer
        // and no overlapped I/O.
        let done = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                0x0009_00A4,
                buffer.as_ptr().cast(),
                buffer.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(done, 0, "reparse point required");
    }
    let (app, root) = app("dist");
    let source = Source::open(app.path(), &root).unwrap();
    fs::write(root.join("data.bin"), b"x").unwrap();
    mark(&root.join("data.bin"));
    assert!(source.read("/data.bin").is_err());
    assert!(Source::open(app.path(), &root).is_err());
}
#[tokio::test]
async fn startup_uses_the_url_pathname_and_requires_an_entry_even_without_windows() {
    let (app, root) = app("dist");
    let options = BridgeOptions {
        entry: Some("/app.js?x=../alef.ktav".into()),
        ..Default::default()
    };
    let bridge = Bridge::with_options(
        Commands::new(),
        Some(crate::AssetInput::Disk {
            application_dir: app.path().to_owned(),
            selected_root: root.clone(),
        }),
        None,
        options,
    )
    .await
    .unwrap();
    assert_eq!(bridge.entry_url.path(), "/app.js");
    bridge.shutdown().await.unwrap();
    fs::remove_file(root.join("index.html")).unwrap();
    assert!(Bridge::new(Commands::new(), Some(&root), None)
        .await
        .is_err());
    fs::remove_dir_all(&root).unwrap();
    assert!(Bridge::new(Commands::new(), Some(&root), None)
        .await
        .is_err());
    assert!(Bridge::new(Commands::new(), None, None).await.is_err());
}
#[tokio::test]
async fn development_ignores_the_embedder_disk_root() {
    let missing = Path::new("nonexistent-application/dist");
    let bridge = Bridge::new(
        Commands::new(),
        Some(missing),
        Some(url::Url::parse("http://127.0.0.1:1/").unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(bridge.entry_url.scheme(), "http");
    assert_eq!(
        read_asset(None, "/".into()).await.unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    bridge.shutdown().await.unwrap();
}
#[cfg(unix)]
fn link(target: &Path, path: &Path) {
    std::os::unix::fs::symlink(target, path).unwrap();
}
#[cfg(windows)]
fn link(target: &Path, path: &Path) {
    let child = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(path.as_os_str().to_string_lossy().replace('/', "\\"))
        .arg(target.as_os_str().to_string_lossy().replace('/', "\\"))
        .spawn()
        .unwrap();
    let status = wait_for_process(child);
    assert!(status.success(), "actual junction required");
}
#[cfg(windows)]
fn wait_for_process(mut child: std::process::Child) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if std::time::Instant::now() >= deadline {
            // Do not follow kill with an unbounded wait; Child's Drop never waits.
            let _ = child.kill();
            panic!("junction process exceeded its 10-second deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn start_worker(
    work: impl FnOnce() + Send + 'static,
) -> std::sync::mpsc::Receiver<std::thread::Result<()>> {
    let (done, completion) = std::sync::mpsc::channel();
    // Deliberately detach: neither success nor timeout performs an unbounded join.
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
        let _ = done.send(result);
    });
    completion
}

fn finish_worker(completion: std::sync::mpsc::Receiver<std::thread::Result<()>>) {
    match completion.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok(Ok(())) => {}
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(error) => panic!("replacement worker did not complete within 60 seconds: {error}"),
    }
}

#[test]
fn selected_and_nested_root_links_are_rejected_but_ancestor_links_are_allowed() {
    let (app, root) = app("dist");
    // The target bears the name of the link: the check of the opened name cannot be what refuses it.
    let twin = tempfile::tempdir().unwrap();
    fs::create_dir(twin.path().join("sub")).unwrap();
    fs::write(twin.path().join("sub/index.html"), b"outside").unwrap();
    link(&twin.path().join("sub"), &root.join("sub"));
    assert!(Source::open(app.path(), &root).is_err());
    #[cfg(windows)]
    fs::remove_dir(root.join("sub")).unwrap();
    #[cfg(unix)]
    fs::remove_file(root.join("sub")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("index.html"), b"outside").unwrap();
    link(outside.path(), &root.join("sub"));
    assert!(Source::open(app.path(), &root).is_err());
    #[cfg(windows)]
    fs::remove_dir(root.join("sub")).unwrap();
    #[cfg(unix)]
    fs::remove_file(root.join("sub")).unwrap();
    fs::remove_dir_all(&root).unwrap();
    link(outside.path(), &root);
    assert!(Source::open(app.path(), &root).is_err());
    fs::create_dir(app.path().join("web")).unwrap();
    link(outside.path(), &app.path().join("web/build"));
    assert!(Source::open(app.path(), &app.path().join("web/build")).is_err());
    let real = tempfile::tempdir().unwrap();
    let application = real.path().join("application");
    let selected = application.join("dist");
    fs::create_dir_all(&selected).unwrap();
    fs::write(selected.join("index.html"), b"public").unwrap();
    let parent = tempfile::tempdir().unwrap();
    link(real.path(), &parent.path().join("ancestor"));
    let boundary = parent.path().join("ancestor/application");
    assert_eq!(
        Source::open(&boundary, &boundary.join("dist"))
            .unwrap()
            .read("/")
            .unwrap()
            .0,
        b"public"
    );
    assert_eq!(
        Source::open(&application, &selected)
            .unwrap()
            .read("/")
            .unwrap()
            .0,
        b"public"
    );
}
#[cfg(windows)]
#[test]
fn the_application_root_must_be_a_directory_not_a_regular_file() {
    let app = tempfile::tempdir().unwrap();
    let file = app.path().join("application");
    fs::write(&file, b"not a directory").unwrap();
    assert!(super::windows::root(&file).is_err());
    assert!(Source::open(&file, &file.join("dist")).is_err());
}

#[cfg(windows)]
#[test]
fn bounded_junction_replacement_never_returns_outside_bytes() {
    let (app, root) = app("dist");
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret"), b"OUTSIDE").unwrap();
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("sub/secret"), b"inside").unwrap();
    let source = Arc::new(Source::open(app.path(), &root).unwrap());
    let root2 = root.clone();
    let outside2 = outside.path().to_owned();
    let fixtures = Arc::new((app, outside));
    let worker_fixtures = Arc::clone(&fixtures);
    let writer = start_worker(move || {
        // Keep fixtures owned by the worker if its completion times out.
        let _fixtures = worker_fixtures;
        for _ in 0..20 {
            fs::rename(root2.join("sub"), root2.join("saved")).unwrap();
            link(&outside2, &root2.join("sub"));
            fs::remove_dir(root2.join("sub")).unwrap();
            fs::rename(root2.join("saved"), root2.join("sub")).unwrap();
        }
    });
    for _ in 0..200 {
        if let Ok((bytes, _)) = source.read("/sub/secret") {
            assert_eq!(bytes, b"inside");
        }
    }
    finish_worker(writer);
    assert_eq!(source.read("/sub/secret").unwrap().0, b"inside");
}
#[cfg(unix)]
#[test]
fn file_links_and_bounded_replacement_races_never_return_outside_bytes() {
    let (app, root) = app("dist");
    fs::write(app.path().join("secret"), b"OUTSIDE").unwrap();
    let source = Arc::new(Source::open(app.path(), &root).unwrap());
    link(&app.path().join("secret"), &root.join("leak"));
    assert!(source.read("/leak").is_err());
    assert!(Source::open(app.path(), &root).is_err());
    fs::remove_file(root.join("leak")).unwrap();
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("sub/file"), b"inside").unwrap();
    let root2 = root.clone();
    let outside = app.path().to_owned();
    let fixture = Arc::new(app);
    let worker_fixture = Arc::clone(&fixture);
    let writer = start_worker(move || {
        // Keep the fixture owned by the worker if its completion times out.
        let _fixture = worker_fixture;
        for _ in 0..100 {
            fs::rename(root2.join("sub"), root2.join("saved")).unwrap();
            link(&outside, &root2.join("sub"));
            fs::remove_file(root2.join("sub")).unwrap();
            fs::rename(root2.join("saved"), root2.join("sub")).unwrap();
        }
    });
    for _ in 0..200 {
        if let Ok((bytes, _)) = source.read("/sub/secret") {
            assert_ne!(bytes, b"OUTSIDE");
        }
        if let Ok((bytes, _)) = source.read("/sub/file") {
            assert_eq!(bytes, b"inside");
        }
    }
    finish_worker(writer);
    assert_eq!(source.read("/sub/file").unwrap().0, b"inside");
}
