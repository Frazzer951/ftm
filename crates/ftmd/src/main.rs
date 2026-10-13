mod state;
mod worktree;

use eyre::{Result, WrapErr};
use ftm_core::{
    paths,
    protocol::{Request, Response},
};
use serde::Serialize;
use state::{Shared, State};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::broadcast::error::RecvError;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let socket = std::path::absolute(paths::socket_path())?;
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir).wrap_err_with(|| format!("creating {}", dir.display()))?;
    }
    if socket.exists() {
        if UnixStream::connect(&socket).await.is_ok() {
            eyre::bail!("ftmd already running on {}", socket.display());
        }
        std::fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket).wrap_err_with(|| format!("binding {}", socket.display()))?;
    tracing::info!("listening on {}", socket.display());

    let state = State::new(socket.clone());
    let mut terminate = signal(SignalKind::terminate())?;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve(stream, state).await {
                        tracing::warn!("client error: {e:#}");
                    }
                });
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = terminate.recv() => break,
        }
    }
    let _ = std::fs::remove_file(&socket);
    Ok(())
}

async fn serve(stream: UnixStream, state: Shared) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        let request = match serde_json::from_str::<Request>(&line) {
            Ok(request) => request,
            Err(e) => {
                let message = format!("bad request: {e}");
                write_line(&mut write, &Response::Error { message }).await?;
                continue;
            },
        };
        if matches!(request, Request::Subscribe) {
            write_line(&mut write, &Response::Ok).await?;
            return stream_events(&mut write, &state).await;
        }
        let shared = state.clone();
        let response = tokio::task::spawn_blocking(move || state::handle(&shared, request)).await?;
        write_line(&mut write, &response).await?;
    }
    Ok(())
}

/// A subscriber that falls behind the broadcast gets a fresh full replay instead of a gap.
async fn stream_events(write: &mut OwnedWriteHalf, state: &Shared) -> Result<()> {
    loop {
        let (mut rx, replay) = state::subscribe(state);
        for event in &replay {
            write_line(write, event).await?;
        }
        loop {
            match rx.recv().await {
                Ok(event) => write_line(write, &event).await?,
                Err(RecvError::Lagged(_)) => break,
                Err(RecvError::Closed) => return Ok(()),
            }
        }
    }
}

async fn write_line(write: &mut OwnedWriteHalf, value: &impl Serialize) -> Result<()> {
    let mut out = serde_json::to_vec(value)?;
    out.push(b'\n');
    write.write_all(&out).await?;
    Ok(())
}
