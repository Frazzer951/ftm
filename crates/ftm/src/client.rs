use eyre::{Result, WrapErr, bail};
use ftm_core::{
    paths,
    protocol::{Request, Response},
};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

pub fn send(request: &Request) -> Result<Response> {
    let socket = paths::socket_path();
    let mut stream = UnixStream::connect(&socket)
        .wrap_err_with(|| format!("connecting to ftmd at {} (is it running?)", socket.display()))?;
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    stream.write_all(&line)?;

    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    match serde_json::from_str(&reply)? {
        Response::Error { message } => bail!(message),
        response => Ok(response),
    }
}
