use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

const ADDRESS: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 8080);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const REQUEST: &[u8] = b"GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";

fn healthy() -> std::io::Result<bool> {
    let mut stream = TcpStream::connect_timeout(&ADDRESS, CONNECT_TIMEOUT)?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.write_all(REQUEST)?;
    stream.flush()?;

    let mut response = [0_u8; 512];
    let mut length = 0;
    while length < response.len() {
        let bytes_read = stream.read(&mut response[length..])?;
        if bytes_read == 0 {
            break;
        }
        length += bytes_read;
        if let Some(line_end) = response[..length].windows(2).position(|window| window == b"\r\n") {
            let status_line = &response[..line_end];
            let mut fields = status_line.split(|byte| *byte == b' ');
            let version_ok = matches!(
                fields.next(),
                Some(version) if version == b"HTTP/1.0" || version == b"HTTP/1.1"
            );
            let status_ok = matches!(fields.next(), Some(status) if status == b"200");
            return Ok(version_ok && status_ok);
        }
    }

    let _ = stream.shutdown(Shutdown::Both);
    Ok(false)
}

fn main() -> ExitCode {
    match healthy() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) | Err(_) => ExitCode::FAILURE,
    }
}
