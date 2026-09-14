//! A blocking connection to the D-Bus session bus over its Unix socket:
//! finding the address, `EXTERNAL` authentication, `Hello`, and whole
//! messages in and out. No async runtime and no thread of its own; callers
//! own the thread. Reads and writes time out after `DBUS_TIMEOUT`.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::path::PathBuf;

use super::wire::{self, FIXED_HEADER_BYTES, Header, Kind, Message, WireError};
use crate::limits::{DBUS_TIMEOUT, MAX_DBUS_ADDRESSES, MAX_DBUS_AUTH_LINE_BYTES, MAX_DBUS_MESSAGES_PER_CALL};

const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";

#[derive(Debug, thiserror::Error)]
pub enum BusError {
    #[error("no session bus address to connect to")]
    NoAddress,
    #[error("session bus: {0}")]
    Io(#[from] std::io::Error),
    #[error("the session bus refused authentication: {0}")]
    Auth(String),
    #[error(transparent)]
    Wire(#[from] WireError),
    #[error("{name}: {message}")]
    Remote { name: String, message: String },
    #[error("no reply from the session bus")]
    NoReply,
}

/// Where a bus listens.
#[derive(Debug, PartialEq, Eq)]
pub enum Address {
    Path(PathBuf),
    Abstract(Vec<u8>),
}

pub struct Bus {
    stream: UnixStream,
    last_serial: u32,
}

impl Bus {
    /// Connects to the session bus, authenticates and says `Hello`.
    pub fn session() -> Result<Self, BusError> {
        let stream = connect(&session_addresses())?;
        stream.set_read_timeout(Some(DBUS_TIMEOUT))?;
        stream.set_write_timeout(Some(DBUS_TIMEOUT))?;
        let mut bus = Self { stream, last_serial: 0 };
        bus.authenticate()?;
        let reply = bus.call(&Header::method_call(DBUS, DBUS_PATH, DBUS, "Hello", ""), &[])?;
        let name = reply.body().str()?;
        if name.is_empty() {
            return Err(WireError::Malformed("empty unique name").into());
        }
        log::debug!("session bus: connected as {name}");
        Ok(bus)
    }

    fn authenticate(&mut self) -> Result<(), BusError> {
        // The uid the bus checks against the socket's peer credentials.
        let uid = std::fs::metadata("/proc/self")?.uid();
        let mut line = Vec::with_capacity(64);
        line.extend_from_slice(b"\0AUTH EXTERNAL ");
        line.extend_from_slice(hex(uid.to_string().as_bytes()).as_bytes());
        line.extend_from_slice(b"\r\n");
        self.stream.write_all(&line)?;
        let reply = self.read_auth_line()?;
        if !reply.starts_with("OK ") {
            return Err(BusError::Auth(reply));
        }
        self.stream.write_all(b"BEGIN\r\n")?;
        Ok(())
    }

    /// Reads one line of the authentication exchange, a byte at a time so
    /// nothing after it is consumed.
    fn read_auth_line(&mut self) -> Result<String, BusError> {
        let mut line = Vec::with_capacity(64);
        let mut byte = [0; 1];
        for _ in 0..MAX_DBUS_AUTH_LINE_BYTES {
            self.stream.read_exact(&mut byte)?;
            if byte[0] == b'\n' && line.last() == Some(&b'\r') {
                line.pop();
                return Ok(String::from_utf8_lossy(&line).into_owned());
            }
            line.push(byte[0]);
        }
        Err(BusError::Auth("reply line too long".to_owned()))
    }

    /// Sends a message; returns its serial.
    pub fn send(&mut self, header: &Header<'_>, body: &[u8]) -> Result<u32, BusError> {
        self.last_serial = self.last_serial.wrapping_add(1).max(1);
        let bytes = wire::encode(header, self.last_serial, body)?;
        self.stream.write_all(&bytes)?;
        Ok(self.last_serial)
    }

    /// Waits for the next message.
    pub fn receive(&mut self) -> Result<Message, BusError> {
        let mut fixed = [0; FIXED_HEADER_BYTES];
        self.stream.read_exact(&mut fixed)?;
        let total = wire::message_len(&fixed)?;
        assert!(total >= FIXED_HEADER_BYTES);
        let mut bytes = vec![0; total];
        bytes[..FIXED_HEADER_BYTES].copy_from_slice(&fixed);
        self.stream.read_exact(&mut bytes[FIXED_HEADER_BYTES..])?;
        Ok(Message::parse(bytes)?)
    }

    /// Calls a method and waits for its reply. Messages arriving meanwhile
    /// (signals such as `NameAcquired`) are dropped.
    pub fn call(&mut self, header: &Header<'_>, body: &[u8]) -> Result<Message, BusError> {
        assert_eq!(header.kind, Kind::MethodCall);
        assert_eq!(header.flags & wire::NO_REPLY_EXPECTED, 0, "a call without a reply can't be waited for");
        let serial = self.send(header, body)?;
        for _ in 0..MAX_DBUS_MESSAGES_PER_CALL {
            let message = self.receive()?;
            if message.reply_serial() != Some(serial) {
                continue;
            }
            if message.kind() == Kind::Error {
                return Err(remote_error(&message));
            }
            return Ok(message);
        }
        Err(BusError::NoReply)
    }
}

fn remote_error(message: &Message) -> BusError {
    let name = message.error_name().unwrap_or("unknown D-Bus error").to_owned();
    let text = if message.signature().starts_with('s') { message.body().str().unwrap_or_default() } else { "" };
    BusError::Remote { name, message: text.to_owned() }
}

fn connect(addresses: &[Address]) -> Result<UnixStream, BusError> {
    let mut last_error = None;
    for address in addresses {
        let result = match address {
            Address::Path(path) => UnixStream::connect(path),
            Address::Abstract(name) => SocketAddr::from_abstract_name(name).and_then(|name| UnixStream::connect_addr(&name)),
        };
        match result {
            Ok(stream) => return Ok(stream),
            Err(error) => {
                log::debug!("session bus at {address:?}: {error}");
                last_error = Some(error);
            }
        }
    }
    Err(last_error.map_or(BusError::NoAddress, BusError::Io))
}

/// The session bus addresses to try, in order: `DBUS_SESSION_BUS_ADDRESS`,
/// else the usual `$XDG_RUNTIME_DIR/bus`.
fn session_addresses() -> Vec<Address> {
    if let Some(text) = std::env::var_os("DBUS_SESSION_BUS_ADDRESS") {
        return parse_addresses(&text.to_string_lossy());
    }
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => vec![Address::Path(PathBuf::from(dir).join("bus"))],
        None => Vec::new(),
    }
}

/// Parses a D-Bus address list ("unix:path=/run/user/1000/bus;tcp:…"),
/// keeping the Unix socket entries catnap can use.
fn parse_addresses(text: &str) -> Vec<Address> {
    text.split(';').take(MAX_DBUS_ADDRESSES).filter_map(parse_address).collect()
}

fn parse_address(entry: &str) -> Option<Address> {
    let params = entry.strip_prefix("unix:")?;
    params.split(',').find_map(|pair| match pair.split_once('=')? {
        ("path", value) => Some(Address::Path(PathBuf::from(OsString::from_vec(unescape(value)?)))),
        ("abstract", value) => Some(Address::Abstract(unescape(value)?)),
        _ => None,
    })
}

/// Undoes the address format's %XX escapes.
fn unescape(value: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(value.len());
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = char::from(bytes.next()?).to_digit(16)?;
            let low = char::from(bytes.next()?).to_digit(16)?;
            out.push(u8::try_from(high * 16 + low).ok()?);
        } else {
            out.push(byte);
        }
    }
    Some(out)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0xf)]));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_socket_addresses_are_kept() {
        assert_eq!(parse_addresses("unix:path=/run/user/1000/bus"), [Address::Path("/run/user/1000/bus".into())]);
        assert_eq!(
            parse_addresses("unix:abstract=/tmp/dbus-AbC,guid=0123"),
            [Address::Abstract(b"/tmp/dbus-AbC".to_vec())]
        );
        assert_eq!(parse_addresses("tcp:host=localhost,port=4;unix:guid=1,path=/a%20b"), [Address::Path("/a b".into())]);
    }

    #[test]
    fn unusable_addresses_are_skipped() {
        assert_eq!(parse_addresses(""), []);
        assert_eq!(parse_addresses("unix:path=/a%2"), []);
        assert_eq!(parse_addresses("unix:path=/a%zz"), []);
        assert_eq!(parse_addresses("unix:tmpdir=/tmp"), []);
    }

    #[test]
    fn external_auth_sends_the_uid_digits_in_hex() {
        assert_eq!(hex(b"1000"), "31303030");
        assert_eq!(hex(&[0x0f, 0xa0]), "0fa0");
    }

    #[test]
    #[ignore = "needs a session bus: cargo test -- --ignored"]
    fn says_hello_to_the_session_bus() {
        Bus::session().unwrap();
    }
}
