//! Native Unix-domain socket transport, including Windows AF_UNIX sockets.

use std::io;
use std::path::Path;

#[cfg(unix)]
pub type Stream = tokio::net::UnixStream;

#[cfg(windows)]
pub type Stream = tokio::net::TcpStream;

/// Connect to the existing Codex socket without creating or removing its path.
#[cfg(unix)]
pub async fn connect(path: &Path) -> io::Result<Stream> {
    match Stream::connect(path).await {
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
            // Recent Codex versions publish a symlink to a shorter physical
            // socket path. Resolve it when the advertised path exceeds sun_path.
            Stream::connect(tokio::fs::canonicalize(path).await?).await
        }
        result => result,
    }
}

/// Windows Codex uses AF_UNIX, not a named pipe. Tokio's Windows socket reactor
/// can drive its connected stream through the common Winsock send/recv API.
#[cfg(windows)]
pub async fn connect(path: &Path) -> io::Result<Stream> {
    use socket2::{Domain, SockAddr, Socket, Type};

    let address = SockAddr::unix(path)?;
    let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    socket.set_nonblocking(true)?;
    let pending = match socket.connect(&address) {
        Ok(()) => false,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => true,
        Err(error) => return Err(error),
    };

    // The conversion transfers sole ownership of a Winsock stream socket. We
    // use only stream I/O and shutdown, never TCP options or IP addresses.
    let stream = Stream::from_std(socket.into())?;
    if pending {
        // Dropping this future closes the socket, including an in-flight connect;
        // no blocking worker survives the caller's connection timeout.
        stream.writable().await?;
        if let Some(error) = stream.take_error()? {
            return Err(error);
        }
    }
    Ok(stream)
}
