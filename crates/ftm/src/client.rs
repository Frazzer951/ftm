use eyre::{Result, WrapErr, bail};
use ftm_core::{
    paths,
    protocol::{Request, Response},
};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

pub fn send(request: &Request) -> Result<Response> {
    let socket = paths::socket_path();
    let mut stream = UnixStream::connect(&socket)
        .wrap_err_with(|| format!("connecting to ftmd at {} (is it running?)", socket.display()))?;
    stream.set_read_timeout(Some(reply_timeout(request)))?;
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    stream.write_all(&line)?;

    let mut reply = String::new();
    BufReader::new(stream)
        .read_line(&mut reply)
        .wrap_err("waiting for ftmd to reply")?;
    if reply.is_empty() {
        bail!("ftmd closed the connection without replying");
    }
    match serde_json::from_str(&reply)? {
        Response::Error { message } => bail!(message),
        response => Ok(response),
    }
}

/// How long ftmd may take to answer, so a hung ftmd never blocks a hook forever. Making a worktree copies files and
/// removing one waits for its panes to exit, so those get longer.
fn reply_timeout(request: &Request) -> Duration {
    match request {
        Request::Spawn(spawn) if spawn.worktree_branch.is_some() => Duration::from_secs(600),
        Request::Close {
            remove_worktree: true, ..
        } => Duration::from_secs(120),
        _ => Duration::from_secs(30),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftm_core::model::{Kind, WorkspaceId};
    use ftm_core::protocol::SpawnRequest;

    #[test]
    fn only_slow_requests_wait_long() {
        let spawn = |worktree_branch: Option<&str>| {
            Request::Spawn(SpawnRequest {
                kind: Kind::Agent,
                name: None,
                parent: None,
                cwd: "/".into(),
                worktree_branch: worktree_branch.map(Into::into),
                base: None,
                copy_changes: false,
                brief: None,
                command: None,
            })
        };
        let close = |remove_worktree| Request::Close {
            id: WorkspaceId::nil(),
            remove_worktree,
        };
        assert_eq!(reply_timeout(&Request::Ping), Duration::from_secs(30));
        assert_eq!(reply_timeout(&spawn(None)), Duration::from_secs(30));
        assert_eq!(reply_timeout(&close(false)), Duration::from_secs(30));
        assert!(reply_timeout(&spawn(Some("b"))) >= Duration::from_secs(300));
        assert!(reply_timeout(&close(true)) >= Duration::from_secs(60));
    }
}
