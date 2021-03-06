
use crypto::digest::Digest;
use crypto::sha1::Sha1;
//use mio::net::tcp::{TcpStream};
use serialize::base64::{Config, Newline, Standard, ToBase64};
use std::collections::HashMap;
use std::error::FromError;
use std::io::{BufRead, BufStream, Error, Read, Write};
use std::net::TcpStream;
use std::num::FromPrimitive;
//use std::option::Option;
//use std::result::Result;

static WS_MAGIC_GUID : &'static str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

pub struct WebSocketStream<S> {
    stream: BufStream<S>
}

#[derive(Debug)]
pub enum WsError { Bitwise, Io(String), Parse(http_grammar::ParseError), Handshake(String), Protocol(String) }

// Move this into a different spot
pub trait TryClone {
    fn try_clone(&self) -> Result<Self, WsError>;
}

impl<S> TryClone for WebSocketStream<S> where S: Read + Write + TryClone {
    fn try_clone(&self) -> Result<WebSocketStream<S>, WsError> {
        Ok(WebSocketStream{stream: try!(self.stream.try_clone())})
    }
}

impl<S> TryClone for BufStream<S> where S: Read + Write + TryClone {
    fn try_clone(&self) -> Result<BufStream<S>, WsError> {
        let inner = self.get_ref();
        let clone_inner = try!(inner.try_clone());
        Ok(BufStream::new(clone_inner))
    }
}

// Move this into another different spot, distinct from above
impl TryClone for TcpStream {
    fn try_clone(&self) -> Result<TcpStream, WsError> {
        Ok(try!(self.try_clone()))
    }
}

impl FromError<Error> for WsError {
    fn from_error(err: Error) -> WsError { WsError::Io(err.to_string()) }
}

impl FromError<http_grammar::ParseError> for WsError {
    fn from_error(err: http_grammar::ParseError) -> WsError { WsError::Parse(err) }
}

pub struct HttpHeader {
    name: String,
    value: String
}

peg! http_grammar (r#"
use protocol::HttpHeader;

name -> String
    = [^:]+ { match_str.to_string() }

value -> String
    = .+ { match_str.to_string() }

#[pub]
header -> HttpHeader
    = n:name ": " v:value { HttpHeader{ name: n, value: v } }
"#);

impl <S: Read + Write + TryClone> WebSocketStream<S> {

    pub fn new(stream: S) -> Result<WebSocketStream<S>, WsError> {

        let mut stream = BufStream::new(stream);

        let mut method_line = String::new();
        try!(stream.read_line(&mut method_line));

        let mut headers = HashMap::new();

        loop {
        	let mut header_line = String::new();
            try!(stream.read_line(&mut header_line));
            let header_line = String::from_str(header_line.trim());
            if header_line.len() == 0 { break }
            let header = try!(http_grammar::header(&header_line));
            headers.insert(header.name, header.value);
        }

        let key = String::from_str("Sec-WebSocket-Key");

        let challenge_response: String = match headers.get(&key) {
            None => return Err(WsError::Handshake(String::from_str("No WebSocket Challenge Header"))),
            Some(challenge) => {
                let mut hasher = Sha1::new();
                let mut local = challenge.clone();
                local.push_str(WS_MAGIC_GUID);
                println!("Using challenge and guid '{}'", local);
                hasher.input(local.as_bytes());
                let mut output = [0;20];
                hasher.result(&mut output);
                output.to_base64(Config {
                    char_set: Standard,
                    line_length: None,
                    newline: Newline::CRLF,
                    pad: true,
                })
            }
        };

        println!("Sending response '{}'", challenge_response);

        let response = format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n", challenge_response);

        try!(stream.write(response.as_bytes()));
        try!(stream.flush());

        Ok(WebSocketStream{stream: stream})

    }

    pub fn send(&mut self, msg: &[u8]) -> Result<(), WsError> {

        try!(self.stream.write(&[129]));

        let len: usize = msg.len();
        let l1: u8 = try!(FromPrimitive::from_usize(len >> 24).ok_or(WsError::Bitwise));
        let l2: u8 = try!(FromPrimitive::from_usize(len >> 16).ok_or(WsError::Bitwise));
        let l3: u8 = try!(FromPrimitive::from_usize(len >>  8).ok_or(WsError::Bitwise));
        let l4: u8 = try!(FromPrimitive::from_usize(len      ).ok_or(WsError::Bitwise));

        try!(match len {
            _ if len <= 125 => 
            self.stream.write(&[l4]),
            _ if len > 125 && len <= 65535 => 
            self.stream.write(&[126u8, l3, l4]),
            _ => 
            // HMM, looks like really 8 bytes are required
            self.stream.write(&[127u8, l1, l2, l3, l4])
        });

        try!(self.stream.write(&msg));
        try!(self.stream.flush());

        Ok(())
    }

    pub fn recv(&mut self) -> Result<Vec<u8>, WsError> {

        let mut text_type:  [u8;1] = [0;1];
        let mut len_buffer: [u8;1] = [0;1];

        try!(self.stream.read(&mut text_type));

        println!("Parsing message with type {:?}", text_type);

        try!(self.stream.read(&mut len_buffer));

        let len1 = len_buffer[0] & 0x7F;

        let length: usize = match len1 {
            _ if len1 <= 125 => {
                try!(FromPrimitive::from_u8(len1).ok_or(WsError::Bitwise))
            },
            _ if len1 == 126 => {
                let mut l: [u8;2] = [0;2];
                try!(self.stream.read(&mut l));
                let high: usize = try!(FromPrimitive::from_u8(l[0]).ok_or(WsError::Bitwise));
                let low: usize = try!(FromPrimitive::from_u8(l[1]).ok_or(WsError::Bitwise));
                (high << 8) | low
            }
            _ =>
                return Err(WsError::Protocol(String::from_str("Message too big, not implemented")))
        };

        println!("Receiving message with {} bytes", length);

        let mut mask: [u8;4] = [0;4];
        try!(self.stream.read(&mut mask));

        let mut data = Vec::new();
        data.resize(length, 0);

        try!(self.stream.read(data.as_mut_slice()));

        for i in (0 .. length) {
            data[i] = data[i] ^ mask[i % 4];
        }

        Ok(data)

    }

}

