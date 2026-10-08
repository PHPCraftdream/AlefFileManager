// SPDX-License-Identifier: MIT OR Apache-2.0
//! The folder the server gives by itself, and the rights it is given under.
use super::*;

/// A folder with a page, a text in a folder, a big file and, above it, a secret.
fn site() -> (tempfile::TempDir, PathBuf) {
    let base = tempfile::tempdir().unwrap();
    let real = base.path().canonicalize().unwrap();
    let real = match real.to_str().and_then(|text| text.strip_prefix(r"\\?\")) {
        Some(rest) => PathBuf::from(rest),
        None => real,
    };
    let root = real.join("public");
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::write(real.join("secret.txt"), "the secret").unwrap();
    std::fs::write(root.join("index.html"), "<h1>home</h1>").unwrap();
    std::fs::write(root.join("a").join("b.txt"), "bee").unwrap();
    std::fs::write(root.join("style.css"), "body {}").unwrap();
    let big: Vec<u8> = (0..1024 * 1024).map(big_byte).collect();
    std::fs::write(root.join("big.bin"), big).unwrap();
    (base, root)
}

fn scope_of(folder: &Path) -> String {
    format!("{}/**", folder.display().to_string().replace('\\', "/"))
}

#[tokio::test]
async fn a_folder_is_served_without_a_word_to_the_page_and_nothing_outside_it() {
    let (base, root) = site();
    let app = app_for(&[scope_of(&root)]).await;
    let folder = root.to_string_lossy().into_owned();
    let mut served = serve(&app, json!({ "files": folder })).await.unwrap();

    let (status, headers, body) = fetch("GET", served.url("/"), Vec::new(), Vec::new())
        .await
        .unwrap();
    assert_eq!(
        (status, body.as_slice()),
        (StatusCode::OK, b"<h1>home</h1>".as_slice())
    );
    assert_eq!(headers["content-type"], "text/html; charset=utf-8");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    let (_, headers, body) = fetch("GET", served.url("/a/b.txt"), Vec::new(), Vec::new())
        .await
        .unwrap();
    assert_eq!(
        (headers["content-type"].to_str().unwrap(), body.as_slice()),
        ("text/plain; charset=utf-8", b"bee".as_slice())
    );
    let (_, headers, _) = fetch("GET", served.url("/style.css"), Vec::new(), Vec::new())
        .await
        .unwrap();
    assert_eq!(headers["content-type"], "text/css; charset=utf-8");
    let (status, headers, body) = fetch("HEAD", served.url("/a/b.txt"), Vec::new(), Vec::new())
        .await
        .unwrap();
    assert_eq!(
        (
            status,
            headers["content-length"].to_str().unwrap(),
            body.len()
        ),
        (StatusCode::OK, "3", 0)
    );
    let (_, _, big) = fetch("GET", served.url("/big.bin"), Vec::new(), Vec::new())
        .await
        .unwrap();
    let expected: Vec<u8> = (0..1024 * 1024).map(big_byte).collect();
    assert!(big == expected, "a big file comes whole");
    assert!(
        nothing_comes(&mut served.requests).await,
        "the page heard of none of this"
    );
    let (status, _, _) = fetch(
        "GET",
        served.url("/index.html"),
        vec![("host", "evil.test".to_owned())],
        Vec::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        status,
        StatusCode::MISDIRECTED_REQUEST,
        "the files are held too"
    );

    // What is not a file of the folder goes to the page, and the way out of the folder is no way.
    for path in [
        "/missing.txt",
        "/..%2fsecret.txt",
        "/%2e%2e/secret.txt",
        "/a/../../secret.txt",
        "/a%5c..%5c..%5csecret.txt",
    ] {
        let call = plain_get(served.url(path));
        let seen = next_request(&mut served.requests)
            .await
            .unwrap_or_else(|| panic!("{path} did not reach the page"));
        respond(&app, seen.id, 404, &[], b"not here").await.unwrap();
        let (status, _, body) = call.await.unwrap().unwrap();
        assert_eq!(
            (status, body.as_slice()),
            (StatusCode::NOT_FOUND, b"not here".as_slice()),
            "{path}"
        );
    }
    let call = tokio::spawn(fetch(
        "POST",
        served.url("/index.html"),
        Vec::new(),
        b"x".to_vec(),
    ));
    let seen = next_request(&mut served.requests)
        .await
        .expect("a POST goes to the page");
    assert_eq!(seen.method, "POST");
    respond(&app, seen.id, 200, &[], b"posted").await.unwrap();
    assert_eq!(call.await.unwrap().unwrap().2, b"posted");
    drop(base);
}

/// A folder `link` that leads to `target`: a symbolic link, or a junction on Windows (it asks no right).
fn link_folder(link: &Path, target: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    {
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(made.success(), "a junction could not be made");
    }
}

#[tokio::test]
async fn a_link_inside_the_folder_to_a_place_outside_it_is_no_way_out() {
    let (base, root) = site();
    let outside = root.parent().unwrap().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("s.txt"), "outside").unwrap();
    link_folder(&root.join("out"), &outside);
    // The scope of the application covers both folders: only the folder of `files` keeps the file in.
    let app = app_for(&[scope_of(root.parent().unwrap())]).await;
    let mut served = serve(&app, json!({ "files": root.to_string_lossy() }))
        .await
        .unwrap();
    let call = plain_get(served.url("/out/s.txt"));
    let seen = next_request(&mut served.requests)
        .await
        .expect("the way out is left to the page");
    respond(&app, seen.id, 404, &[], b"no").await.unwrap();
    assert_eq!(call.await.unwrap().unwrap().2, b"no");
    drop(base);
}

#[tokio::test]
async fn a_file_the_user_gave_a_stand_in_for_is_not_given_while_the_rest_are() {
    let (_base, root) = site();
    std::fs::create_dir_all(root.join("hidden")).unwrap();
    std::fs::write(root.join("hidden").join("h.txt"), "hidden").unwrap();
    let everything = scope_of(&root);
    let hidden = scope_of(&root.join("hidden"));
    let mut consent = Consent::undecided();
    consent.set(Right::scoped("fs.read", &everything), Decision::Allow);
    consent.set(Right::scoped("fs.read", &hidden), Decision::Substitute);
    consent.set(Right::scoped("net.socket", LISTEN), Decision::Allow);
    let app = app_for(&[everything, hidden]).await.with_consent(consent);
    let mut served = serve(&app, json!({ "files": root.to_string_lossy() }))
        .await
        .unwrap();
    let (status, _, body) = fetch("GET", served.url("/a/b.txt"), Vec::new(), Vec::new())
        .await
        .unwrap();
    assert_eq!(
        (status, body.as_slice()),
        (StatusCode::OK, b"bee".as_slice())
    );
    let call = plain_get(served.url("/hidden/h.txt"));
    let seen = next_request(&mut served.requests)
        .await
        .expect("the file with a stand-in is left to the page");
    respond(&app, seen.id, 200, &[], b"stand-in").await.unwrap();
    assert_eq!(call.await.unwrap().unwrap().2, b"stand-in");
}

#[tokio::test]
async fn a_folder_the_application_may_not_read_is_not_served() {
    let (_base, root) = site();
    let app = app_for(&[]).await;
    let denied = app
        .call(
            "http.serve",
            json!({ "port": 0, "files": root.to_string_lossy() }),
        )
        .await;
    assert_eq!(denied.unwrap_err().code, ErrorCode::PermissionDenied);

    let file = root.join("index.html");
    let allowed = app_for(&[scope_of(&root)]).await;
    let not_a_folder = allowed
        .call(
            "http.serve",
            json!({ "port": 0, "files": file.to_string_lossy() }),
        )
        .await;
    assert_eq!(not_a_folder.unwrap_err().code, ErrorCode::InvalidArgument);

    // A folder that the user gave a stand-in for has no files: everything goes to the page.
    let mut consent = Consent::undecided();
    consent.set(
        Right::scoped("fs.read", &scope_of(&root)),
        Decision::Substitute,
    );
    consent.set(Right::scoped("net.socket", LISTEN), Decision::Allow);
    let stood_in = app_for(&[scope_of(&root)]).await.with_consent(consent);
    let mut served = serve(&stood_in, json!({ "files": root.to_string_lossy() }))
        .await
        .unwrap();
    let call = plain_get(served.url("/index.html"));
    let seen = next_request(&mut served.requests)
        .await
        .expect("the page is asked");
    respond(&stood_in, seen.id, 404, &[], b"none")
        .await
        .unwrap();
    assert_eq!(call.await.unwrap().unwrap().0, StatusCode::NOT_FOUND);
    // The folder the page sees need not be the real one: a stand-in of a folder that is not there serves.
    let missing = root.join("missing");
    let served = serve(&stood_in, json!({ "files": missing.to_string_lossy() })).await;
    assert!(served.is_ok(), "{:?}", served.err());
}
