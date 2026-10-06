// SPDX-License-Identifier: MIT OR Apache-2.0
mod backend;
mod storage;

use std::io;
use std::path::PathBuf;
use url::Url;

surfman::declare_surfman!();

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut root = std::env::current_dir()?;
    let mut frontend = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../frontend/dist");
    let mut development_url = None;
    let mut data_directory = None;
    let mut arguments = std::env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--root") => {
                root = arguments
                    .next()
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "--root requires a directory")
                    })?
                    .into()
            }
            Some("--frontend-dir") => {
                frontend = arguments
                    .next()
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "--frontend-dir requires a directory",
                        )
                    })?
                    .into()
            }
            Some("--data-dir") => {
                data_directory = Some(PathBuf::from(arguments.next().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--data-dir requires a directory",
                    )
                })?))
            }
            Some("--frontend-url") => {
                let value = arguments.next().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "--frontend-url requires a URL")
                })?;
                let url = Url::parse(&value.to_string_lossy())?;
                if url.scheme() != "http"
                    || url.host_str() != Some("127.0.0.1")
                    || !url.username().is_empty()
                    || url.password().is_some()
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Development frontend must be an HTTP server on 127.0.0.1",
                    )
                    .into());
                }
                development_url = Some(url);
            }
            Some("--help" | "-h") => {
                println!("alef-file-manager [--root DIRECTORY] [--data-dir DATABASE_DIRECTORY] [--frontend-dir BUILT_UI] [--frontend-url HTTP_LOOPBACK_URL]\nEmbedded Servo with asynchronous Rust backend, Fjall storage and React frontend.");
                return Ok(());
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Unknown argument: {}", argument.to_string_lossy()),
                )
                .into())
            }
        }
    }
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| io::Error::other("Failed to configure the Servo TLS provider"))?;
    let assets = if development_url.is_some() {
        None
    } else {
        Some(frontend.as_path())
    };
    let data_directory = match data_directory {
        Some(directory) => directory,
        None => dirs::data_local_dir()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "Local application data directory is unavailable",
                )
            })?
            .join("AlefFileManager")
            .join("fjall"),
    };
    let storage = storage::Storage::open(&data_directory).await?;
    let commands = backend::commands(&root, storage).await?;
    let mut bridge = alef_runtime::Bridge::new(commands, assets, development_url).await?;
    eprintln!("ALEF_READY private-bridge pid={}", std::process::id());
    let result = alef_runtime::run(
        &mut bridge,
        alef_runtime::WindowOptions::new(
            "Alef File Manager",
            include_bytes!("../../frontend/public/logo-32x32.png").to_vec(),
        )
        .decorations(false),
    );
    bridge.shutdown().await?;
    result?;
    Ok(())
}
