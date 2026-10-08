//! Acknowledged generic-Netlink and XFRM requests on bounded blocking workers.
use netlink_sys::{Socket, SocketAddr};
use std::io;
use std::os::fd::AsRawFd;
use tokio::sync::Semaphore;

pub(super) const REQUEST: u16 = 1 | 4;
pub(super) const CREATE: u16 = REQUEST | 0x200 | 0x400;
pub(super) const NESTED: u16 = 0x8000;
static WORKERS: Semaphore = Semaphore::const_new(32);

pub(super) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    let permit = WORKERS
        .acquire()
        .await
        .expect("native worker admission stays open");
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(io::Error::other)?
}

pub(super) fn attr(kind: u16, value: &[u8]) -> Vec<u8> {
    let len = value.len() + 4;
    let mut data = Vec::with_capacity((len + 3) & !3);
    data.extend_from_slice(&(len as u16).to_ne_bytes());
    data.extend_from_slice(&kind.to_ne_bytes());
    data.extend_from_slice(value);
    data.resize((len + 3) & !3, 0);
    data
}

pub(super) fn attrs(data: &[u8]) -> io::Result<Vec<(u16, &[u8])>> {
    let mut result = Vec::new();
    let mut offset = 0;
    while offset < data.len() {
        if data.len() - offset < 4 {
            return Err(io::Error::other("truncated netlink attribute"));
        }
        let len = u16::from_ne_bytes(data[offset..offset + 2].try_into().unwrap()) as usize;
        let kind = u16::from_ne_bytes(data[offset + 2..offset + 4].try_into().unwrap());
        if len < 4 || len > data.len() - offset {
            return Err(io::Error::other("invalid netlink attribute length"));
        }
        result.push((kind & 0x3fff, &data[offset + 4..offset + len]));
        offset += (len + 3) & !3;
    }
    Ok(result)
}

pub(super) struct NativeSocket {
    socket: Socket,
    sequence: u32,
}

impl NativeSocket {
    pub(super) fn new(protocol: isize) -> io::Result<Self> {
        let mut socket = Socket::new(protocol)?;
        if protocol == libc::NETLINK_ROUTE as isize {
            socket.set_netlink_get_strict_chk(true)?;
        }
        socket.bind_auto()?;
        socket.connect(&SocketAddr::new(0, 0))?;
        let timeout = libc::timeval {
            tv_sec: 30,
            tv_usec: 0,
        };
        // A kernel ACK timeout must release the worker and leave rollback to its owner.
        let result = unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                (&timeout as *const libc::timeval).cast(),
                size_of::<libc::timeval>() as _,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            socket,
            sequence: 0,
        })
    }

    pub(super) fn request(&mut self, kind: u16, flags: u16, body: &[u8]) -> io::Result<Vec<u8>> {
        Ok(self
            .exchange(kind, flags, body)?
            .into_iter()
            .flatten()
            .collect())
    }

    pub(super) fn dump(&mut self, kind: u16, body: &[u8]) -> io::Result<Vec<Vec<u8>>> {
        self.exchange(kind, REQUEST | 0x300, body)
    }

    fn exchange(&mut self, kind: u16, flags: u16, body: &[u8]) -> io::Result<Vec<Vec<u8>>> {
        self.sequence = self.sequence.wrapping_add(1);
        let mut message = Vec::with_capacity(16 + body.len());
        message.extend_from_slice(&((16 + body.len()) as u32).to_ne_bytes());
        message.extend_from_slice(&kind.to_ne_bytes());
        message.extend_from_slice(&flags.to_ne_bytes());
        message.extend_from_slice(&self.sequence.to_ne_bytes());
        message.extend_from_slice(&0u32.to_ne_bytes());
        message.extend_from_slice(body);
        self.socket.send(&message, 0)?;
        let mut reply = Vec::new();
        loop {
            let (data, sender) = self.socket.recv_from_full()?;
            if sender.port_number() != 0 {
                continue;
            }
            let mut offset = 0;
            while offset < data.len() {
                if data.len() - offset < 16 {
                    return Err(io::Error::other("truncated netlink header"));
                }
                let header = &data[offset..offset + 16];
                let len = u32::from_ne_bytes(header[..4].try_into().unwrap()) as usize;
                if len < 16 || len > data.len() - offset {
                    return Err(io::Error::other("invalid netlink message length"));
                }
                let response_kind = u16::from_ne_bytes(header[4..6].try_into().unwrap());
                let response_flags = u16::from_ne_bytes(header[6..8].try_into().unwrap());
                let sequence = u32::from_ne_bytes(header[8..12].try_into().unwrap());
                let payload = &data[offset + 16..offset + len];
                if sequence == self.sequence {
                    if response_flags & 0x10 != 0 {
                        return Err(io::Error::other("netlink dump interrupted"));
                    }
                    if response_kind == 3 {
                        if let Some(code) = payload.get(..4) {
                            let error = i32::from_ne_bytes(code.try_into().unwrap());
                            if error != 0 {
                                return Err(io::Error::from_raw_os_error(-error));
                            }
                        }
                        return Ok(reply);
                    }
                    if response_kind == 2 {
                        let code = payload
                            .get(..4)
                            .ok_or_else(|| io::Error::other("truncated netlink ACK"))?;
                        let error = i32::from_ne_bytes(code.try_into().unwrap());
                        if error != 0 {
                            return Err(io::Error::from_raw_os_error(-error));
                        }
                        if flags & 0x300 != 0x300 {
                            return Ok(reply);
                        }
                    } else {
                        reply.push(payload.to_vec());
                    }
                }
                offset += (len + 3) & !3;
            }
        }
    }

    pub(super) fn generic(
        &mut self,
        family: u16,
        command: u8,
        version: u8,
        attributes: &[u8],
    ) -> io::Result<Vec<u8>> {
        let mut body = vec![command, version, 0, 0];
        body.extend_from_slice(attributes);
        let reply = self.request(family, REQUEST, &body)?;
        Ok(reply.get(4..).unwrap_or_default().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_preserve_alignment_and_nested_flags() {
        let data = [attr(2 | NESTED, &[1]), attr(7, &[2, 3, 4, 5])].concat();
        assert_eq!(
            attrs(&data).unwrap(),
            vec![(2, &[1][..]), (7, &[2, 3, 4, 5][..])]
        );
        assert!(attrs(&[0, 0, 1, 0]).is_err());
        assert!(attrs(&[8, 0, 1, 0, 1]).is_err());
    }
}
