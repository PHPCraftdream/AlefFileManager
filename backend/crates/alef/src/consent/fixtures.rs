// SPDX-License-Identifier: MIT OR Apache-2.0
//! Application folders for the tests of the launcher.
use std::path::{Path, PathBuf};

use crate::plan::MANIFEST_FILE;

/// The folder `<dir>/<id>` with a manifest that asks for `clipboard.read` and the variable `HOME`.
pub fn app(dir: &Path, id: &str) -> PathBuf {
    let folder = dir.join(id);
    std::fs::create_dir_all(&folder).unwrap();
    let manifest = format!(
        "id: {id}
name: Example
version:: 1.0.0
windows: [
    {{
        label: main
        url: /index.html
        width: 800
        height: 600
    }}
]
external: {{
    connect: []
    load: {{
        scripts: []
        styles: []
        images: []
        fonts: []
        media: []
        frames: []
    }}
}}
permissions: {{
    fs: {{
        read: []
        write: []
    }}
    cli: {{
        exec: []
    }}
    net: {{
        http: []
        socket: []
    }}
    shell: {{
        openExternal: []
    }}
    clipboard: {{
        read: true
    }}
    shortcut: {{
        global: false
    }}
    secrets: false
    app: {{
        env: [ HOME ]
    }}
}}
"
    );
    std::fs::write(folder.join(MANIFEST_FILE), manifest).unwrap();
    folder
}
