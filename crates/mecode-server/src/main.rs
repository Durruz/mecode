//! `mecode` — запускает ядро и сервер, открывает интерфейс в браузере.
//!
//! Флаги: `--port <N>`, `--data-dir <путь>`, `--no-open`.

use mecode_core::{Config, Hub};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut args = std::env::args().skip(1);
    let mut port = None;
    let mut data_dir = mecode_server::default_data_dir();
    let mut open = true;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => port = args.next().and_then(|p| p.parse().ok()),
            "--data-dir" => data_dir = args.next().map(Into::into).unwrap_or(data_dir),
            "--no-open" => open = false,
            "-h" | "--help" => {
                println!("mecode [--port N] [--data-dir PATH] [--no-open]");
                return Ok(());
            }
            other => anyhow::bail!("неизвестный аргумент: {other}"),
        }
    }

    let cfg = Config::load_or_create(&data_dir)?;
    let port = port.unwrap_or(cfg.port);
    let hub = Hub::new(cfg, &data_dir)?;
    let (addr, server) = mecode_server::serve(hub, port).await?;
    let url = format!("http://{addr}");
    println!("Mecode: {url}");
    println!("Данные и настройки: {}", data_dir.display());

    if open {
        #[cfg(windows)]
        let _ = std::process::Command::new("cmd").args(["/C", "start", "", &url]).spawn();
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg(&url).spawn();
        #[cfg(all(unix, not(target_os = "macos")))]
        let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
    }

    tokio::select! {
        _ = server => {}
        _ = tokio::signal::ctrl_c() => println!("Выход."),
    }
    Ok(())
}
