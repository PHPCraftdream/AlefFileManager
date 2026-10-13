// SPDX-License-Identifier: MIT OR Apache-2.0
//! Когда требование `runtime` манифеста не удовлетворено, лаунчер перезапускается в сохранённую
//! копию себя из хранилища и пересылает код её выхода; помеченный потомок, всё ещё не
//! удовлетворяющий требованию, завершается прежней диагностикой.
use std::process::Command;

const MANIFEST: &str = r#"id: org.example.relaunch
name: Example
version:: 1.0.0
runtime: ^999
windows: [
    {
        label: main
        url: /
        width: 800
        height: 600
    }
]
external: {
    connect: []
    load: {
        scripts: []
        styles: []
        images: []
        fonts: []
        media: []
        frames: []
    }
}
permissions: {
    fs: {
        read: []
        write: []
    }
    cli: {
        exec: []
    }
    net: {
        http: []
        socket: []
    }
    shell: {
        openExternal: []
    }
    clipboard: {
        read: false
    }
    shortcut: {
        global: false
    }
    secrets: false
    app: {
        env: []
    }
}
"#;

fn bounded_exit(child: &mut std::process::Child, note: &str) -> i32 {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        match child.try_wait().expect("child state is readable") {
            Some(status) => return status.code().unwrap_or(-1),
            None => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("{note}: did not exit in time");
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }
}

#[test]
fn an_unmet_requirement_relaunches_into_the_stored_binary_and_forwards_its_exit_code() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let store = home.join("runtimes");
    let installed = alef_launch::runtimes::install::install_in(
        &store,
        "999.0.0",
        std::path::Path::new(env!("CARGO_BIN_EXE_alef")),
    )
    .unwrap();
    let app = temp.path().join("app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("alef.ktav"), MANIFEST).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_alef"))
        .arg("--app")
        .arg(&app)
        .env("ALEF_HOME", &home)
        .env_remove("ALEF_E2E")
        .env_remove("ALEF_RELAUNCHED")
        .env_remove("ALEF_RELAUNCHED_FROM")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let code = bounded_exit(&mut child, "relaunched launcher");
    assert_eq!(code, 2);
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    use std::io::Read;
    let mut stdout_text = String::new();
    let mut stderr_text = String::new();
    stdout.read_to_string(&mut stdout_text).unwrap();
    stderr.read_to_string(&mut stderr_text).unwrap();
    assert_eq!(stderr_text.matches("RUNTIME_INCOMPATIBLE").count(), 1);
    assert!(stderr_text.contains("requires ^999"));
    assert!(stderr_text.contains("999.0.0"));
    let expected_selection = format!(
        "selected installed runtime: 999.0.0 at {}",
        installed.display()
    );
    assert!(stderr_text.contains(&expected_selection));
    assert!(stdout_text.is_empty());
}
