//! D-Bus wire format: whole messages in and out, and the values inside them.
//! Pure, no I/O. Bad input gives an error, never a panic; only bad values
//! from catnap itself trip an assertion. Little-endian only: catnap sends
//! it, and the desktops it targets are little-endian, so a big-endian
//! message is refused rather than converted.

use std::cmp::Ordering;
use std::ops::Range;

use crate::limits::{MAX_DBUS_DEPTH, MAX_DBUS_HEADER_FIELDS, MAX_DBUS_MESSAGE_BYTES};

/// Endianness, type, flags, version, body length, serial and the length of
/// the header field array: what's needed to know how long a message is.
pub const FIXED_HEADER_BYTES: usize = 16;
/// Header flag: the sender wants no reply.
pub const NO_REPLY_EXPECTED: u8 = 0x1;
const LITTLE_ENDIAN: u8 = b'l';
const PROTOCOL_VERSION: u8 = 1;
const MAX_SIGNATURE_BYTES: usize = 255;

const FIELD_PATH: u8 = 1;
const FIELD_INTERFACE: u8 = 2;
const FIELD_MEMBER: u8 = 3;
const FIELD_ERROR_NAME: u8 = 4;
const FIELD_REPLY_SERIAL: u8 = 5;
const FIELD_DESTINATION: u8 = 6;
const FIELD_SENDER: u8 = 7;
const FIELD_SIGNATURE: u8 = 8;
const FIELD_UNIX_FDS: u8 = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    MethodCall,
    MethodReturn,
    Error,
    Signal,
}

impl Kind {
    fn code(self) -> u8 {
        match self {
            Self::MethodCall => 1,
            Self::MethodReturn => 2,
            Self::Error => 3,
            Self::Signal => 4,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::MethodCall),
            2 => Some(Self::MethodReturn),
            3 => Some(Self::Error),
            4 => Some(Self::Signal),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    #[error("D-Bus message is truncated")]
    Truncated,
    #[error("D-Bus message is over {max} bytes", max = MAX_DBUS_MESSAGE_BYTES)]
    TooLarge,
    #[error("D-Bus message is big-endian or not protocol version 1")]
    Unsupported,
    #[error("D-Bus signature nests deeper than {max} levels", max = MAX_DBUS_DEPTH)]
    TooDeep,
    #[error("malformed D-Bus message: {0}")]
    Malformed(&'static str),
}

/// The header fields each message type must carry.
fn required_fields(kind: Kind) -> &'static [u8] {
    match kind {
        Kind::MethodCall => &[FIELD_PATH, FIELD_MEMBER],
        Kind::MethodReturn => &[FIELD_REPLY_SERIAL],
        Kind::Error => &[FIELD_ERROR_NAME, FIELD_REPLY_SERIAL],
        Kind::Signal => &[FIELD_PATH, FIELD_INTERFACE, FIELD_MEMBER],
    }
}

/// The type every known header field must have.
fn field_signature(code: u8) -> Option<&'static str> {
    match code {
        FIELD_PATH => Some("o"),
        FIELD_INTERFACE | FIELD_MEMBER | FIELD_ERROR_NAME | FIELD_DESTINATION | FIELD_SENDER => Some("s"),
        FIELD_REPLY_SERIAL | FIELD_UNIX_FDS => Some("u"),
        FIELD_SIGNATURE => Some("g"),
        _ => None,
    }
}

/// The header of a message to send. `signature` describes the body ("" for none).
#[derive(Debug, Clone, Copy)]
pub struct Header<'a> {
    pub kind: Kind,
    pub flags: u8,
    pub path: Option<&'a str>,
    pub interface: Option<&'a str>,
    pub member: Option<&'a str>,
    pub error_name: Option<&'a str>,
    pub reply_serial: Option<u32>,
    pub destination: Option<&'a str>,
    pub signature: &'a str,
}

impl<'a> Header<'a> {
    /// A method call that wants a reply.
    pub fn method_call(destination: &'a str, path: &'a str, interface: &'a str, member: &'a str, signature: &'a str) -> Self {
        Self {
            kind: Kind::MethodCall,
            flags: 0,
            path: Some(path),
            interface: Some(interface),
            member: Some(member),
            error_name: None,
            reply_serial: None,
            destination: Some(destination),
            signature,
        }
    }

    /// The reply to a method call, sent back to its sender.
    pub fn method_return(reply_serial: u32, destination: Option<&'a str>, signature: &'a str) -> Self {
        Self {
            kind: Kind::MethodReturn,
            flags: 0,
            path: None,
            interface: None,
            member: None,
            error_name: None,
            reply_serial: Some(reply_serial),
            destination,
            signature,
        }
    }

    /// An error reply to a method call. Its body is one string: the message.
    pub fn error(reply_serial: u32, destination: Option<&'a str>, error_name: &'a str) -> Self {
        Self { kind: Kind::Error, error_name: Some(error_name), signature: "s", ..Self::method_return(reply_serial, destination, "") }
    }

    /// A signal to whoever listens.
    pub fn signal(path: &'a str, interface: &'a str, member: &'a str, signature: &'a str) -> Self {
        Self {
            kind: Kind::Signal,
            flags: 0,
            path: Some(path),
            interface: Some(interface),
            member: Some(member),
            error_name: None,
            reply_serial: None,
            destination: None,
            signature,
        }
    }

    fn has(&self, code: u8) -> bool {
        match code {
            FIELD_PATH => self.path.is_some(),
            FIELD_INTERFACE => self.interface.is_some(),
            FIELD_MEMBER => self.member.is_some(),
            FIELD_ERROR_NAME => self.error_name.is_some(),
            FIELD_REPLY_SERIAL => self.reply_serial.is_some(),
            _ => false,
        }
    }
}

/// Encodes a whole message. Fails only when it would be too large.
pub fn encode(header: &Header<'_>, serial: u32, body: &[u8]) -> Result<Vec<u8>, WireError> {
    assert!(serial != 0, "D-Bus serial 0 is reserved");
    assert!(required_fields(header.kind).iter().all(|&code| header.has(code)), "incomplete {:?} header", header.kind);
    assert_eq!(header.signature.is_empty(), body.is_empty(), "a body comes with a signature, and only a body");
    if body.len() > MAX_DBUS_MESSAGE_BYTES {
        return Err(WireError::TooLarge);
    }
    let mut writer = Writer::with_capacity(128 + body.len());
    writer.u8(LITTLE_ENDIAN);
    writer.u8(header.kind.code());
    writer.u8(header.flags);
    writer.u8(PROTOCOL_VERSION);
    writer.u32(len_u32(body.len()));
    writer.u32(serial);
    let fields = writer.begin_array(8);
    write_fields(&mut writer, header);
    writer.end_array(fields);
    writer.pad(8);
    writer.buf.extend_from_slice(body);
    if writer.buf.len() > MAX_DBUS_MESSAGE_BYTES {
        return Err(WireError::TooLarge);
    }
    Ok(writer.buf)
}

/// Each field is a struct: its code, then its value as a variant.
fn write_fields(writer: &mut Writer, header: &Header<'_>) {
    if let Some(path) = header.path {
        writer.begin_struct();
        writer.u8(FIELD_PATH);
        writer.variant("o");
        writer.object_path(path);
    }
    let strings = [
        (FIELD_INTERFACE, header.interface),
        (FIELD_MEMBER, header.member),
        (FIELD_ERROR_NAME, header.error_name),
        (FIELD_DESTINATION, header.destination),
    ];
    for (code, value) in strings {
        if let Some(value) = value {
            writer.begin_struct();
            writer.u8(code);
            writer.variant("s");
            writer.str(value);
        }
    }
    if let Some(reply_serial) = header.reply_serial {
        writer.begin_struct();
        writer.u8(FIELD_REPLY_SERIAL);
        writer.variant("u");
        writer.u32(reply_serial);
    }
    if !header.signature.is_empty() {
        writer.begin_struct();
        writer.u8(FIELD_SIGNATURE);
        writer.variant("g");
        writer.signature(header.signature);
    }
}

fn len_u32(len: usize) -> u32 {
    assert!(len <= MAX_DBUS_MESSAGE_BYTES, "D-Bus value of {len} bytes");
    u32::try_from(len).expect("MAX_DBUS_MESSAGE_BYTES fits in a u32")
}

pub fn is_object_path(path: &str) -> bool {
    if path == "/" {
        return true;
    }
    let Some(rest) = path.strip_prefix('/') else { return false };
    rest.split('/')
        .all(|segment| !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
}

/// Appends D-Bus values, padded as the spec requires. Alignment counts from
/// the buffer's start, which is an 8-byte boundary of the message for both
/// whole messages and bodies.
#[derive(Debug, Default)]
pub struct Writer {
    buf: Vec<u8>,
}

/// An array whose length `Writer::end_array` fills in.
#[derive(Debug)]
#[must_use = "close every array with Writer::end_array"]
pub struct OpenArray {
    len_at: usize,
    elements_at: usize,
}

impl Writer {
    pub fn with_capacity(bytes: usize) -> Self {
        Self { buf: Vec::with_capacity(bytes) }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    fn pad(&mut self, align: usize) {
        assert!(matches!(align, 1 | 2 | 4 | 8), "alignment {align}");
        let padded = self.buf.len().next_multiple_of(align);
        self.buf.resize(padded, 0);
    }

    pub fn u8(&mut self, value: u8) {
        self.buf.push(value);
    }

    pub fn bool(&mut self, value: bool) {
        self.u32(u32::from(value));
    }

    pub fn i32(&mut self, value: i32) {
        self.pad(4);
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    pub fn u32(&mut self, value: u32) {
        self.pad(4);
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    pub fn str(&mut self, value: &str) {
        assert!(!value.contains('\0'), "D-Bus strings can't hold NUL");
        self.u32(len_u32(value.len()));
        self.buf.extend_from_slice(value.as_bytes());
        self.buf.push(0);
    }

    pub fn object_path(&mut self, value: &str) {
        assert!(is_object_path(value), "invalid object path {value:?}");
        self.str(value);
    }

    pub fn signature(&mut self, value: &str) {
        assert!(value.len() <= MAX_SIGNATURE_BYTES && value.is_ascii(), "invalid signature {value:?}");
        self.u8(u8::try_from(value.len()).expect("asserted to fit in a byte"));
        self.buf.extend_from_slice(value.as_bytes());
        self.buf.push(0);
    }

    /// A byte array ("ay").
    pub fn bytes(&mut self, value: &[u8]) {
        let array = self.begin_array(1);
        self.buf.extend_from_slice(value);
        self.end_array(array);
    }

    /// Opens an array of elements aligned to `element_align` (8 for structs
    /// and dict entries). The padding before the first element is written
    /// even when the array stays empty.
    pub fn begin_array(&mut self, element_align: usize) -> OpenArray {
        self.u32(0);
        let len_at = self.buf.len() - 4;
        self.pad(element_align);
        OpenArray { len_at, elements_at: self.buf.len() }
    }

    /// Closes an array, using up its handle so it can't be closed twice. Its
    /// length counts the elements, not the padding.
    pub fn end_array(&mut self, OpenArray { len_at, elements_at }: OpenArray) {
        assert!(len_at + 4 <= elements_at && elements_at <= self.buf.len());
        let len = len_u32(self.buf.len() - elements_at);
        self.buf[len_at..len_at + 4].copy_from_slice(&len.to_le_bytes());
    }

    /// Starts a struct or dict entry (both 8-byte aligned); its fields follow.
    pub fn begin_struct(&mut self) {
        self.pad(8);
    }

    /// Starts a variant: its signature. One value of that type follows.
    pub fn variant(&mut self, signature: &str) {
        self.signature(signature);
    }
}

/// Reads D-Bus values from a buffer that starts at an 8-byte boundary of
/// its message. Every read is bounds-checked.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

/// Where an array being read ends (see `Reader::array`).
#[derive(Debug, Clone, Copy)]
pub struct ArrayEnd(usize);

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn align(&mut self, align: usize) -> Result<(), WireError> {
        debug_assert!(matches!(align, 1 | 2 | 4 | 8), "alignment {align}");
        let padded = self.pos.next_multiple_of(align);
        let padding = self.bytes.get(self.pos..padded).ok_or(WireError::Truncated)?;
        if padding.iter().any(|&byte| byte != 0) {
            return Err(WireError::Malformed("non-zero padding"));
        }
        self.pos = padded;
        Ok(())
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], WireError> {
        let end = self.pos.checked_add(len).ok_or(WireError::Truncated)?;
        let taken = self.bytes.get(self.pos..end).ok_or(WireError::Truncated)?;
        self.pos = end;
        Ok(taken)
    }

    fn word(&mut self) -> Result<[u8; 4], WireError> {
        self.align(4)?;
        let mut word = [0; 4];
        word.copy_from_slice(self.take(4)?);
        Ok(word)
    }

    pub fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }

    pub fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_le_bytes(self.word()?))
    }

    pub fn i32(&mut self) -> Result<i32, WireError> {
        Ok(i32::from_le_bytes(self.word()?))
    }

    #[cfg(test)]
    pub fn bool(&mut self) -> Result<bool, WireError> {
        match self.u32()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(WireError::Malformed("boolean isn't 0 or 1")),
        }
    }

    pub fn str(&mut self) -> Result<&'a str, WireError> {
        Ok(self.text()?.1)
    }

    pub fn signature(&mut self) -> Result<&'a str, WireError> {
        Ok(self.signature_text()?.1)
    }

    /// A string (`s` or `o`): where its text sits in the buffer, and the text.
    fn text(&mut self) -> Result<(Range<usize>, &'a str), WireError> {
        let len = usize::try_from(self.u32()?).map_err(|_| WireError::TooLarge)?;
        let start = self.pos;
        let text = std::str::from_utf8(self.take(len)?).map_err(|_| WireError::Malformed("string isn't UTF-8"))?;
        if text.contains('\0') || self.u8()? != 0 {
            return Err(WireError::Malformed("string isn't NUL-terminated"));
        }
        Ok((start..start + len, text))
    }

    /// A signature (`g`): where its text sits in the buffer, and the text.
    fn signature_text(&mut self) -> Result<(Range<usize>, &'a str), WireError> {
        let len = usize::from(self.u8()?);
        let start = self.pos;
        let bytes = self.take(len)?;
        if !bytes.is_ascii() || bytes.contains(&0) || self.u8()? != 0 {
            return Err(WireError::Malformed("bad signature"));
        }
        let text = std::str::from_utf8(bytes).map_err(|_| WireError::Malformed("bad signature"))?;
        Ok((start..start + len, text))
    }

    /// Opens an array; read elements while `more` says so.
    pub fn array(&mut self, element_align: usize) -> Result<ArrayEnd, WireError> {
        let len = usize::try_from(self.u32()?).map_err(|_| WireError::TooLarge)?;
        if len > MAX_DBUS_MESSAGE_BYTES {
            return Err(WireError::TooLarge);
        }
        self.align(element_align)?;
        let end = self.pos + len;
        if end > self.bytes.len() {
            return Err(WireError::Truncated);
        }
        Ok(ArrayEnd(end))
    }

    /// Whether elements remain before `end`.
    pub fn more(&self, end: ArrayEnd) -> Result<bool, WireError> {
        match self.pos.cmp(&end.0) {
            Ordering::Less => Ok(true),
            Ordering::Equal => Ok(false),
            Ordering::Greater => Err(WireError::Malformed("an element overruns its array")),
        }
    }

    pub fn begin_struct(&mut self) -> Result<(), WireError> {
        self.align(8)
    }

    /// Skips one value of each complete type in `signature`.
    pub fn skip(&mut self, signature: &str) -> Result<(), WireError> {
        let mut rest = signature.as_bytes();
        // Every value uses up at least one byte of the signature.
        for _ in 0..signature.len() {
            if rest.is_empty() {
                break;
            }
            rest = self.skip_one(rest, 0)?;
        }
        debug_assert!(rest.is_empty());
        Ok(())
    }

    /// Skips one value of the first complete type in `signature`; returns the rest.
    fn skip_one<'s>(&mut self, signature: &'s [u8], depth: usize) -> Result<&'s [u8], WireError> {
        if depth > MAX_DBUS_DEPTH {
            return Err(WireError::TooDeep);
        }
        let (&code, rest) = signature.split_first().ok_or(WireError::Malformed("missing type in signature"))?;
        match code {
            b'a' => {
                let element_len = complete_type_len(rest, depth + 1)?;
                let end = self.array(alignment(rest[0]))?;
                self.pos = end.0;
                return Ok(&rest[element_len..]);
            }
            b'(' | b'{' => return self.skip_fields(code, rest, depth),
            b'v' => {
                let inner = self.signature()?;
                if !self.skip_one(inner.as_bytes(), depth + 1)?.is_empty() {
                    return Err(WireError::Malformed("variant holds more than one value"));
                }
            }
            b's' | b'o' => {
                self.text()?;
            }
            b'g' => {
                self.signature_text()?;
            }
            _ => {
                let size = fixed_size(code).ok_or(WireError::Malformed("unknown type in signature"))?;
                self.align(size)?;
                self.take(size)?;
            }
        }
        Ok(rest)
    }

    /// Skips a struct's or dict entry's fields, up to the closing bracket.
    fn skip_fields<'s>(&mut self, open: u8, signature: &'s [u8], depth: usize) -> Result<&'s [u8], WireError> {
        self.align(8)?;
        let close = if open == b'(' { b')' } else { b'}' };
        let mut rest = signature;
        for _ in 0..signature.len() {
            match rest.split_first() {
                Some((&next, after)) if next == close => return Ok(after),
                Some(_) => rest = self.skip_one(rest, depth + 1)?,
                None => break,
            }
        }
        Err(WireError::Malformed("unclosed struct in signature"))
    }
}

/// Length of the first complete type in `signature` ("a{sv}i" gives 5).
fn complete_type_len(signature: &[u8], depth: usize) -> Result<usize, WireError> {
    if depth > MAX_DBUS_DEPTH {
        return Err(WireError::TooDeep);
    }
    match signature.first() {
        None => Err(WireError::Malformed("missing type in signature")),
        Some(b'a') => Ok(1 + complete_type_len(&signature[1..], depth + 1)?),
        Some(&open @ (b'(' | b'{')) => {
            let close = if open == b'(' { b')' } else { b'}' };
            let mut len = 1;
            for _ in 0..signature.len() {
                match signature.get(len) {
                    Some(&next) if next == close => return Ok(len + 1),
                    Some(_) => len += complete_type_len(&signature[len..], depth + 1)?,
                    None => break,
                }
            }
            Err(WireError::Malformed("unclosed struct in signature"))
        }
        Some(_) => Ok(1),
    }
}

fn alignment(code: u8) -> usize {
    match code {
        b'n' | b'q' => 2,
        b'b' | b'i' | b'u' | b'h' | b's' | b'o' | b'a' => 4,
        b'x' | b't' | b'd' | b'(' | b'{' => 8,
        // y, g and v; unknown codes are rejected when read.
        _ => 1,
    }
}

fn fixed_size(code: u8) -> Option<usize> {
    match code {
        b'y' => Some(1),
        b'n' | b'q' => Some(2),
        b'b' | b'i' | b'u' | b'h' => Some(4),
        b'x' | b't' | b'd' => Some(8),
        _ => None,
    }
}

/// Total length of the message that starts with `fixed`, so a reader knows
/// how much more to read.
pub fn message_len(fixed: &[u8; FIXED_HEADER_BYTES]) -> Result<usize, WireError> {
    if fixed[0] != LITTLE_ENDIAN || fixed[3] != PROTOCOL_VERSION {
        return Err(WireError::Unsupported);
    }
    let body = word_at(fixed, 4)?;
    let fields = word_at(fixed, 12)?;
    let total = FIXED_HEADER_BYTES
        .checked_add(fields)
        .map(|header| header.next_multiple_of(8))
        .and_then(|header| header.checked_add(body))
        .ok_or(WireError::TooLarge)?;
    if total > MAX_DBUS_MESSAGE_BYTES {
        return Err(WireError::TooLarge);
    }
    Ok(total)
}

fn word_at(fixed: &[u8; FIXED_HEADER_BYTES], at: usize) -> Result<usize, WireError> {
    let word = u32::from_le_bytes([fixed[at], fixed[at + 1], fixed[at + 2], fixed[at + 3]]);
    usize::try_from(word).map_err(|_| WireError::TooLarge)
}

/// The header fields catnap reads, as ranges into the message.
#[derive(Debug, Default)]
struct Fields {
    path: Option<Range<usize>>,
    interface: Option<Range<usize>>,
    member: Option<Range<usize>>,
    error_name: Option<Range<usize>>,
    reply_serial: Option<u32>,
    signature: Option<Range<usize>>,
    sender: Option<Range<usize>>,
    body_at: usize,
}

impl Fields {
    fn has(&self, code: u8) -> bool {
        match code {
            FIELD_PATH => self.path.is_some(),
            FIELD_INTERFACE => self.interface.is_some(),
            FIELD_MEMBER => self.member.is_some(),
            FIELD_ERROR_NAME => self.error_name.is_some(),
            FIELD_REPLY_SERIAL => self.reply_serial.is_some(),
            _ => false,
        }
    }
}

fn read_fields(bytes: &[u8]) -> Result<Fields, WireError> {
    let mut reader = Reader { bytes, pos: 12 };
    let end = reader.array(8)?;
    let mut fields = Fields::default();
    for _ in 0..MAX_DBUS_HEADER_FIELDS {
        if !reader.more(end)? {
            break;
        }
        reader.begin_struct()?;
        let code = reader.u8()?;
        let signature = reader.signature()?;
        match field_signature(code) {
            Some(expected) if expected != signature => return Err(WireError::Malformed("header field of the wrong type")),
            _ => {}
        }
        match code {
            FIELD_PATH => fields.path = Some(reader.text()?.0),
            FIELD_INTERFACE => fields.interface = Some(reader.text()?.0),
            FIELD_MEMBER => fields.member = Some(reader.text()?.0),
            FIELD_ERROR_NAME => fields.error_name = Some(reader.text()?.0),
            FIELD_REPLY_SERIAL => fields.reply_serial = Some(reader.u32()?),
            FIELD_SIGNATURE => fields.signature = Some(reader.signature_text()?.0),
            FIELD_SENDER => fields.sender = Some(reader.text()?.0),
            // Destination, fd count, and codes from later spec versions.
            _ => reader.skip(signature)?,
        }
    }
    if reader.more(end)? {
        return Err(WireError::Malformed("too many header fields"));
    }
    reader.align(8)?;
    fields.body_at = reader.pos;
    Ok(fields)
}

/// A received message. It keeps its bytes; header strings are ranges into them.
#[derive(Debug)]
pub struct Message {
    bytes: Vec<u8>,
    kind: Kind,
    flags: u8,
    serial: u32,
    fields: Fields,
}

impl Message {
    pub fn parse(bytes: Vec<u8>) -> Result<Self, WireError> {
        let fixed = bytes.first_chunk::<FIXED_HEADER_BYTES>().ok_or(WireError::Truncated)?;
        let total = message_len(fixed)?;
        match bytes.len().cmp(&total) {
            Ordering::Less => return Err(WireError::Truncated),
            Ordering::Greater => return Err(WireError::Malformed("bytes after the message")),
            Ordering::Equal => {}
        }
        let kind = Kind::from_code(bytes[1]).ok_or(WireError::Malformed("unknown message type"))?;
        let flags = bytes[2];
        let serial = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        if serial == 0 {
            return Err(WireError::Malformed("serial 0"));
        }
        let fields = read_fields(&bytes)?;
        if !required_fields(kind).iter().all(|&code| fields.has(code)) {
            return Err(WireError::Malformed("required header field missing"));
        }
        if fields.signature.is_none() && fields.body_at < bytes.len() {
            return Err(WireError::Malformed("body without a signature"));
        }
        Ok(Self { bytes, kind, flags, serial, fields })
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn flags(&self) -> u8 {
        self.flags
    }

    pub fn serial(&self) -> u32 {
        self.serial
    }

    /// The sender's unique name; the bus fills it in.
    pub fn sender(&self) -> Option<&str> {
        self.text(self.fields.sender.as_ref())
    }

    pub fn path(&self) -> Option<&str> {
        self.text(self.fields.path.as_ref())
    }

    pub fn interface(&self) -> Option<&str> {
        self.text(self.fields.interface.as_ref())
    }

    pub fn member(&self) -> Option<&str> {
        self.text(self.fields.member.as_ref())
    }

    pub fn reply_serial(&self) -> Option<u32> {
        self.fields.reply_serial
    }

    pub fn error_name(&self) -> Option<&str> {
        self.text(self.fields.error_name.as_ref())
    }

    /// The body's signature, "" when there's no body.
    pub fn signature(&self) -> &str {
        self.text(self.fields.signature.as_ref()).unwrap_or("")
    }

    pub fn body(&self) -> Reader<'_> {
        Reader::new(&self.bytes[self.fields.body_at..])
    }

    /// Text checked when the message was parsed.
    fn text(&self, range: Option<&Range<usize>>) -> Option<&str> {
        std::str::from_utf8(self.bytes.get(range?.clone())?).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DBUS: &str = "org.freedesktop.DBus";

    fn hello() -> Header<'static> {
        Header::method_call(DBUS, "/org/freedesktop/DBus", DBUS, "Hello", "")
    }

    fn reply(reply_serial: u32, signature: &str, body: &[u8]) -> Vec<u8> {
        let header = Header {
            kind: Kind::MethodReturn,
            flags: 0,
            path: None,
            interface: None,
            member: None,
            error_name: None,
            reply_serial: Some(reply_serial),
            destination: Some(":1.42"),
            signature,
        };
        encode(&header, 9, body).unwrap()
    }

    #[test]
    fn hello_is_laid_out_as_the_spec_says() {
        let bytes = encode(&hello(), 1, &[]).unwrap();
        // Little-endian, method call, no flags, version 1, no body, serial 1,
        // then 109 bytes of header fields, padded to 128.
        assert_eq!(bytes[..16], [b'l', 1, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 109, 0, 0, 0]);
        assert_eq!(bytes.len(), 128);
        // The first field: path (1), signature "o", then the string.
        assert_eq!(bytes[16..24], [1, 1, b'o', 0, 21, 0, 0, 0]);
        assert_eq!(&bytes[24..46], b"/org/freedesktop/DBus\0");
        let fixed: &[u8; 16] = bytes.first_chunk().unwrap();
        assert_eq!(message_len(fixed), Ok(128));
    }

    #[test]
    fn encoded_messages_parse_back() {
        let mut body = Writer::default();
        body.str("hi");
        body.u32(7);
        let header = Header::method_call("a.b", "/a/b", "a.b", "M", "su");
        let message = Message::parse(encode(&header, 5, &body.into_bytes()).unwrap()).unwrap();
        assert_eq!(message.kind(), Kind::MethodCall);
        assert_eq!(message.signature(), "su");
        assert_eq!(message.text(message.fields.member.as_ref()), Some("M"));
        assert_eq!(message.text(message.fields.path.as_ref()), Some("/a/b"));
        let mut reader = message.body();
        assert_eq!(reader.str(), Ok("hi"));
        assert_eq!(reader.u32(), Ok(7));
        assert_eq!(reader.u8(), Err(WireError::Truncated));
    }

    #[test]
    fn replies_carry_the_serial_they_answer() {
        let mut body = Writer::default();
        body.u32(3);
        let message = Message::parse(reply(12, "u", &body.into_bytes())).unwrap();
        assert_eq!(message.kind(), Kind::MethodReturn);
        assert_eq!(message.reply_serial(), Some(12));
        assert_eq!(message.body().u32(), Ok(3));
        assert_eq!(message.error_name(), None);
    }

    #[test]
    fn array_length_excludes_the_padding_before_elements() {
        let mut writer = Writer::default();
        writer.u8(1);
        let empty = writer.begin_array(8);
        writer.end_array(empty);
        // Byte, padding to 4, length 0, padding to 8 even though it's empty.
        assert_eq!(writer.buf, [1, 0, 0, 0, 0, 0, 0, 0]);
        let full = writer.begin_array(8);
        writer.begin_struct();
        writer.i32(-1);
        writer.u32(2);
        writer.end_array(full);
        assert_eq!(writer.buf[8..12], [8, 0, 0, 0]);
        assert_eq!(writer.buf.len(), 24);
    }

    #[test]
    fn skip_steps_over_nested_values() {
        let mut writer = Writer::default();
        let dict = writer.begin_array(8);
        writer.begin_struct();
        writer.str("a");
        writer.variant("i");
        writer.i32(1);
        writer.begin_struct();
        writer.str("b");
        writer.variant("as");
        let strings = writer.begin_array(4);
        writer.str("x");
        writer.end_array(strings);
        writer.end_array(dict);
        writer.begin_struct();
        writer.i32(5);
        let empty = writer.begin_array(8);
        writer.end_array(empty);
        let variants = writer.begin_array(1);
        writer.variant("(i)");
        writer.begin_struct();
        writer.i32(6);
        writer.end_array(variants);
        writer.u32(42);
        let bytes = writer.into_bytes();
        let mut reader = Reader::new(&bytes);
        reader.skip("a{sv}(ia{sv}av)").unwrap();
        assert_eq!(reader.u32(), Ok(42));
        assert_eq!(complete_type_len(b"a{sv}i", 0), Ok(5));
    }

    #[test]
    fn foreign_or_cut_messages_are_refused() {
        let bytes = encode(&hello(), 1, &[]).unwrap();
        assert_eq!(Message::parse(bytes[..100].to_vec()).unwrap_err(), WireError::Truncated);
        let mut big_endian = bytes.clone();
        big_endian[0] = b'B';
        assert_eq!(Message::parse(big_endian).unwrap_err(), WireError::Unsupported);
        let mut huge = bytes.clone();
        huge[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(Message::parse(huge).unwrap_err(), WireError::TooLarge);
        let mut longer = bytes;
        longer.push(0);
        assert!(matches!(Message::parse(longer), Err(WireError::Malformed(_))));
    }

    #[test]
    fn a_call_without_its_required_fields_is_malformed() {
        // A valid reply relabelled as a method call has no path or member.
        let mut bytes = reply(1, "", &[]);
        bytes[1] = 1;
        assert_eq!(Message::parse(bytes).unwrap_err(), WireError::Malformed("required header field missing"));
    }

    #[test]
    fn bad_values_are_errors_not_panics() {
        // Length 2, "hi", but no terminating NUL.
        let mut reader = Reader::new(&[2, 0, 0, 0, b'h', b'i', b'!']);
        assert!(matches!(reader.str(), Err(WireError::Malformed(_))));
        let mut reader = Reader::new(&[7, 1, 0, 0, 0, 0, 0, 0]);
        reader.u8().unwrap();
        assert_eq!(reader.u32(), Err(WireError::Malformed("non-zero padding")));
        let deep = "a".repeat(MAX_DBUS_DEPTH + 2) + "i";
        assert_eq!(Reader::new(&[0; 64]).skip(&deep), Err(WireError::TooDeep));
        assert!(matches!(Reader::new(&[0; 8]).skip("z"), Err(WireError::Malformed(_))));
        assert!(matches!(Reader::new(&[0; 8]).skip("(i"), Err(WireError::Malformed(_))));
    }

    #[test]
    fn object_paths() {
        for good in ["/", "/org", "/org/freedesktop/DBus", "/a_1/B2"] {
            assert!(is_object_path(good), "{good}");
        }
        for bad in ["", "org", "/org/", "//", "/a-b", "/a//b"] {
            assert!(!is_object_path(bad), "{bad}");
        }
    }
}
