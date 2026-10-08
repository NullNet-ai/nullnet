//! Fixed gateway exceptions and direct forwarding for fresh endpoint devices.
use super::native_netlink::{CREATE, NESTED, NativeSocket, attr, blocking};
use std::{io, net::Ipv4Addr};

fn message(index: u32, parent: u32, priority: u32, protocol: u16, handle: u32) -> Vec<u8> {
    [
        vec![0; 4],
        index.to_ne_bytes().to_vec(),
        handle.to_ne_bytes().to_vec(),
        parent.to_ne_bytes().to_vec(),
        ((priority << 16) | u32::from(protocol.to_be()))
            .to_ne_bytes()
            .to_vec(),
    ]
    .concat()
}
fn action(kind: &[u8], code: u32, extra: &[u8]) -> Vec<u8> {
    let mut parameters = vec![0; 20];
    parameters[8..12].copy_from_slice(&code.to_ne_bytes());
    attr(
        1 | NESTED,
        &[
            attr(1, kind),
            attr(2 | NESTED, &[attr(2, &parameters), extra.to_vec()].concat()),
        ]
        .concat(),
    )
}
fn redirect(index: u32) -> Vec<u8> {
    let mut parameters = vec![0; 28];
    parameters[8..12].copy_from_slice(&4u32.to_ne_bytes());
    parameters[20..24].copy_from_slice(&1u32.to_ne_bytes());
    parameters[24..28].copy_from_slice(&index.to_ne_bytes());
    attr(
        1 | NESTED,
        &[
            attr(1, b"mirred\0"),
            attr(2 | NESTED, &attr(2, &parameters)),
        ]
        .concat(),
    )
}
#[allow(clippy::too_many_arguments)]
fn filter(
    socket: &mut NativeSocket,
    index: u32,
    priority: u32,
    protocol: u16,
    handle: u32,
    kind: &[u8],
    options: Vec<u8>,
    parent: u32,
) -> io::Result<()> {
    socket.request(
        44,
        CREATE,
        &[
            message(index, parent, priority, protocol, handle),
            attr(1, kind),
            attr(2 | NESTED, &options),
        ]
        .concat(),
    )?;
    Ok(())
}
fn gateway(
    socket: &mut NativeSocket,
    index: u32,
    ip: Ipv4Addr,
    protocol: u16,
    priority: u32,
    mark: Option<u32>,
) -> io::Result<()> {
    let mut selector = vec![0; 16];
    selector[0] = 1;
    selector[2] = 1;
    selector.extend(u32::MAX.to_be_bytes());
    selector.extend(ip.octets());
    selector.extend((if protocol == 0x800 { 16i32 } else { 24i32 }).to_ne_bytes());
    selector.extend(0i32.to_ne_bytes());
    let mut options = [
        attr(5, &selector),
        attr(7 | NESTED, &action(b"gact\0", 0, &[])),
    ]
    .concat();
    if let Some(mark) = mark {
        options.extend(attr(
            10,
            &[
                mark.to_ne_bytes(),
                u32::MAX.to_ne_bytes(),
                0u32.to_ne_bytes(),
            ]
            .concat(),
        ));
    }
    filter(
        socket, index, priority, protocol, 0, b"u32\0", options, 0xfffffff2,
    )
}

pub(super) async fn install(
    transport: u32,
    outer: Option<u32>,
    ip: Ipv4Addr,
    encrypted_id: Option<u32>,
) -> io::Result<()> {
    blocking(move || {
        let mut socket = NativeSocket::new(libc::NETLINK_ROUTE as _)?;
        let mark = encrypted_id.map(super::native_xfrm::mark);
        for index in [Some(transport), outer].into_iter().flatten() {
            socket.request(
                36,
                CREATE,
                &[
                    message(index, 0xfffffff1, 0, 3, 0xffff0000),
                    attr(1, b"clsact\0"),
                ]
                .concat(),
            )?;
        }
        if let Some(mark) = mark {
            let marking = action(
                b"skbedit\0",
                3,
                &[
                    attr(5, &mark.to_ne_bytes()),
                    attr(8, &u32::MAX.to_ne_bytes()),
                ]
                .concat(),
            );
            filter(
                &mut socket,
                transport,
                1,
                3,
                0,
                b"matchall\0",
                attr(2 | NESTED, &marking),
                0xfffffff3,
            )?;
        }
        if let Some(outer) = outer {
            for (priority, protocol) in [(1, 0x800), (2, 0x806)] {
                gateway(&mut socket, outer, ip, protocol, priority, None)?;
                gateway(&mut socket, transport, ip, protocol, priority, mark)?;
            }
            filter(
                &mut socket,
                outer,
                3,
                3,
                0,
                b"matchall\0",
                attr(2 | NESTED, &redirect(transport)),
                0xfffffff2,
            )?;
        }
        let receive = outer
            .map(redirect)
            .unwrap_or_else(|| action(b"gact\0", 0, &[]));
        if let Some(mark) = mark {
            filter(
                &mut socket,
                transport,
                3,
                3,
                mark,
                b"fw\0",
                attr(4 | NESTED, &receive),
                0xfffffff2,
            )?;
            filter(
                &mut socket,
                transport,
                4,
                3,
                0,
                b"matchall\0",
                attr(2 | NESTED, &action(b"gact\0", 2, &[])),
                0xfffffff2,
            )?;
        } else if outer.is_some() {
            filter(
                &mut socket,
                transport,
                3,
                3,
                0,
                b"matchall\0",
                attr(2 | NESTED, &receive),
                0xfffffff2,
            )?;
        }
        Ok(())
    })
    .await
}
