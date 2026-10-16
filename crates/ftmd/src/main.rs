mod frame;
mod holder;
mod hooks;
mod state;
mod upgrade;
mod worktree;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use eyre::{Result, WrapErr};
use ftm_core::model::PaneId;
use ftm_core::{
    paths,
    protocol::{Event, Request, Response},
};
use serde::Serialize;
use state::{Shared, State};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::broadcast::error::RecvError;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("pty-holder") {
        return holder::run(&args[2..]);
    }
    if args.get(1).map(String::as_str) == Some(upgrade::HANDSHAKE) {
        upgrade::print_handshake();
        return Ok(());
    }
    let inherited = unsafe { upgrade::Inherited::take() };
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let result = runtime.block_on(daemon(inherited));
    // A request stuck on a pane must not keep ftmd from exiting.
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    result
}

async fn daemon(inherited: Option<upgrade::Inherited>) -> Result<()> {
    tracing_subscriber::fmt::init();

    let socket = std::path::absolute(paths::socket_path())?;
    if let Some(dir) = socket.parent() {
        paths::private_dir(dir, paths::socket_dir_is_ours(dir)).wrap_err_with(|| format!("creating {}", dir.display()))?;
    }
    let listener = match &inherited {
        Some(inherited) => {
            let listener = UnixListener::from_std(inherited.listener(&socket)?)?;
            tracing::info!("taking over {} (pid {})", socket.display(), std::process::id());
            listener
        },
        None => {
            if socket.exists() {
                if UnixStream::connect(&socket).await.is_ok() {
                    eyre::bail!("ftmd already running on {}", socket.display());
                }
                std::fs::remove_file(&socket)?;
            }
            let listener = UnixListener::bind(&socket).wrap_err_with(|| format!("binding {}", socket.display()))?;
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
            tracing::info!("listening on {} (pid {})", socket.display(), std::process::id());
            listener
        },
    };

    let claude_settings = hooks::write(&socket).map(Some).unwrap_or_else(|e| {
        tracing::warn!("agents start without status hooks: {e:#}");
        None
    });
    let state = State::new(socket.clone(), claude_settings);
    if let Some(inherited) = inherited {
        state::check_saved(&state)?;
        tokio::task::spawn_blocking(move || inherited.ready()).await??;
    }
    let loading = state.clone();
    tokio::task::spawn_blocking(move || state::load(&loading)).await??;
    let mut upgrades = upgrade::watch(state.clone());
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
            Some(candidate) = async { upgrades.as_mut()?.recv().await } => {
                tracing::info!("moving onto ftmd {}", candidate.version);
                let Err(e) = upgrade::hand_over(&candidate.exe, listener.as_raw_fd(), &state);
                tracing::warn!("staying on this ftmd: {e:#}");
                state::update_blocked(&state, Some(candidate.version), format!("the new ftmd did not take over: {e:#}"));
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = terminate.recv() => break,
        }
    }
    // Panes live on in their holders; the next ftmd reconnects to them.
    let _ = std::fs::remove_file(&socket);
    Ok(())
}

async fn serve(stream: UnixStream, state: Shared) -> Result<()> {
    let peer = stream.peer_cred().ok().and_then(|c| c.pid());
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
        if let Request::Attach { pane } = request {
            return stream_pane(&mut write, &state, pane).await;
        }
        // Killing a pane leaves no other trace of who asked.
        if matches!(request, Request::Close { .. } | Request::ClosePane { .. }) {
            tracing::info!("{request:?} from pid {}", peer.map_or("?".into(), |p| p.to_string()));
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

async fn stream_pane(write: &mut OwnedWriteHalf, state: &Shared, pane: PaneId) -> Result<()> {
    let mut first = true;
    loop {
        let (mut rx, screen) = match state::attach(state, pane) {
            Ok(attached) => attached,
            Err(e) if first => {
                let message = format!("{e:#}");
                return write_line(write, &Response::Error { message }).await;
            },
            Err(_) => return Ok(()),
        };
        if first {
            write_line(write, &Response::Ok).await?;
            first = false;
        }
        let redraw = Event::Output {
            pane,
            data: B64.encode(screen),
            reset: true,
        };
        write_line(write, &redraw).await?;
        loop {
            match rx.recv().await {
                Ok(event @ Event::Output { pane: p, .. }) if p == pane => write_line(write, &event).await?,
                Ok(Event::Workspaces { workspaces }) if !state::pane_alive(&workspaces, pane) => return Ok(()),
                Ok(_) => {},
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
