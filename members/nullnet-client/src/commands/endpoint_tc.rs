//! Shared classifier infrastructure with fresh per-interface ownership.
use super::native_netlink::{CREATE, NESTED, NativeSocket, REQUEST, attr, blocking};
use aya::{Ebpf, maps::HashMap as AyaHashMap, maps::MapData, programs::SchedClassifier};
use std::{
    io,
    net::Ipv4Addr,
    os::fd::{AsFd, AsRawFd},
    sync::Arc,
    sync::Mutex,
};
use tokio::sync::OnceCell;

#[path = "../../../../shared/endpoint_tc.rs"]
mod config;
use config::EndpointConfig;
unsafe impl aya::Pod for EndpointConfig {}

pub(super) const HOLDER_GROUP: u32 = 0x4e60_0002;
const BLOCK_BASE: u32 = 7801;
const BLOCK_PAIRS: u32 = 16;
static STATE: OnceCell<State> = OnceCell::const_new();

pub(super) fn holder_name(name: &str) -> bool {
    name.strip_prefix("nn-tc-")
        .and_then(|s| s.parse::<u32>().ok())
        .is_some_and(|pair| pair < BLOCK_PAIRS && name == format!("nn-tc-{pair}"))
}

struct State {
    map: Mutex<AyaHashMap<MapData, u32, EndpointConfig>>,
    _bpf: Ebpf,
    holders: Vec<u32>,
}

impl Drop for State {
    fn drop(&mut self) {
        if let Ok(mut route) = NativeSocket::new(libc::NETLINK_ROUTE as _) {
            for index in &self.holders {
                let _ = route.request(17, REQUEST, &super::endpoint_io::info(*index, false));
            }
        }
    }
}

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

fn qdisc(route: &mut NativeSocket, index: u32, pair: u32) -> io::Result<()> {
    route.request(
        36,
        CREATE,
        &[
            message(index, 0xfffffff1, 0, 3, 0xffff0000),
            attr(1, b"clsact\0"),
            attr(13, &(BLOCK_BASE + pair * 2).to_ne_bytes()),
            attr(14, &(BLOCK_BASE + pair * 2 + 1).to_ne_bytes()),
        ]
        .concat(),
    )?;
    Ok(())
}

fn load() -> io::Result<State> {
    let mut bpf = Ebpf::load(aya::include_bytes_aligned!(env!("NULLNET_BIN_PATH")))
        .map_err(io::Error::other)?;
    let mut fds = Vec::new();
    for name in ["nullnet_endpoint_ingress", "nullnet_endpoint_egress"] {
        let program: &mut SchedClassifier = bpf
            .program_mut(name)
            .ok_or_else(|| io::Error::other("Endpoint classifier missing"))?
            .try_into()
            .map_err(io::Error::other)?;
        program.load().map_err(io::Error::other)?;
        fds.push(program.fd().map_err(io::Error::other)?.as_fd().as_raw_fd());
    }
    let map = bpf
        .take_map("ENDPOINT_CONFIG")
        .ok_or_else(|| io::Error::other("Endpoint configuration map missing"))?
        .try_into()
        .map_err(io::Error::other)?;
    let mut state = State {
        map: Mutex::new(map),
        _bpf: bpf,
        holders: Vec::new(),
    };
    let mut route = NativeSocket::new(libc::NETLINK_ROUTE as _)?;
    for pair in 0..BLOCK_PAIRS {
        let name = format!("nn-tc-{pair}");
        let link = route.request(
            16,
            CREATE | 8,
            &[
                super::endpoint_io::info(0, false),
                attr(3, format!("{name}\0").as_bytes()),
                attr(27, &HOLDER_GROUP.to_ne_bytes()),
                attr(18 | NESTED, &attr(1, b"dummy\0")),
            ]
            .concat(),
        )?;
        let index = u32::from_ne_bytes(
            link.get(4..8)
                .ok_or_else(|| io::Error::other("Classifier holder lacks interface index"))?
                .try_into()
                .unwrap(),
        );
        state.holders.push(index);
        qdisc(&mut route, index, pair)?;
        for (direction, fd) in fds.iter().enumerate() {
            let block = BLOCK_BASE + pair * 2 + direction as u32;
            // Shared filters use TCM_IFINDEX_MAGIC_BLOCK and tcm_block_index.
            route.request(
                44,
                CREATE,
                &[
                    message(u32::MAX, block, 1, 3, 1),
                    attr(1, b"bpf\0"),
                    attr(
                        2 | NESTED,
                        &[
                            attr(6, &fd.to_ne_bytes()),
                            attr(7, b"nullnet-endpoint\0"),
                            attr(8, &1u32.to_ne_bytes()),
                        ]
                        .concat(),
                    ),
                ]
                .concat(),
            )?;
        }
    }
    Ok(state)
}

pub(super) async fn initialize() -> io::Result<()> {
    STATE.get_or_try_init(|| blocking(load)).await?;
    Ok(())
}

pub(super) struct Ownership {
    indices: Vec<u32>,
    active: Mutex<bool>,
}

impl Ownership {
    pub(super) fn revoke(&self) -> io::Result<()> {
        let mut active = self.active.lock().unwrap();
        if !*active {
            return Ok(());
        }
        let mut map = STATE.get().unwrap().map.lock().unwrap();
        let mut result = Ok(());
        for index in &self.indices {
            if let Err(error) = map.remove(index)
                && !matches!(&error, aya::maps::MapError::SyscallError(e)
                    if e.io_error.raw_os_error() == Some(libc::ENOENT))
            {
                result = Err(io::Error::other(error));
            }
        }
        if result.is_ok() {
            *active = false;
        }
        result
    }
}

impl Drop for Ownership {
    fn drop(&mut self) {
        if let Err(error) = self.revoke() {
            eprintln!("Endpoint classifier rollback failed: {error}");
        }
    }
}

pub(super) async fn install(
    transport: u32,
    outer: Option<u32>,
    ip: Ipv4Addr,
    encrypted_id: Option<u32>,
) -> io::Result<Arc<Ownership>> {
    initialize().await?;
    blocking(move || {
        let mut ownership = Ownership {
            indices: Vec::new(),
            active: Mutex::new(true),
        };
        let mut route = NativeSocket::new(libc::NETLINK_ROUTE as _)?;
        let mark = encrypted_id.map(super::native_xfrm::mark).unwrap_or(0);
        for (index, output, mark) in [
            (Some(transport), outer.unwrap_or(0), mark),
            (outer, transport, 0),
        ] {
            let Some(index) = index else { continue };
            qdisc(&mut route, index, index % BLOCK_PAIRS)?;
            STATE
                .get()
                .unwrap()
                .map
                .lock()
                .unwrap()
                .insert(
                    index,
                    EndpointConfig {
                        gateway: u32::from_ne_bytes(ip.octets()),
                        output,
                        mark,
                        padding: 0,
                    },
                    1,
                )
                .map_err(io::Error::other)?;
            ownership.indices.push(index);
        }
        Ok(Arc::new(ownership))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use aya::programs::{TestRun, TestRunOptions};

    fn run(name: &str, index: u32, packet: &[u8], mark: u32) -> (u32, u32) {
        let program: &SchedClassifier = STATE
            .get()
            .unwrap()
            ._bpf
            .program(name)
            .unwrap()
            .try_into()
            .unwrap();
        let mut context = [0u8; 192];
        context[8..12].copy_from_slice(&mark.to_ne_bytes());
        context[40..44].copy_from_slice(&index.to_ne_bytes());
        let mut output = [0u8; 192];
        let result = program
            .test_run(TestRunOptions {
                data_in: Some(packet),
                ctx_in: Some(&context),
                ctx_out: Some(&mut output),
                ..Default::default()
            })
            .unwrap();
        (
            result.return_value,
            u32::from_ne_bytes(output[8..12].try_into().unwrap()),
        )
    }

    #[tokio::test]
    #[ignore = "requires root and an isolated root network namespace"]
    async fn shared_classifier_enforces_marks_and_revokes_failed_and_retired_owners() {
        let root = super::super::RtNetLinkHandle::new().unwrap();
        let mut indices = Vec::new();
        for name in ["nnct_transport", "nnct_outer"] {
            root.handle
                .link()
                .add(rtnetlink::LinkDummy::new(name).build())
                .execute()
                .await
                .unwrap();
            indices.push(
                super::super::netlink::get_link_by_name(&root.handle, name)
                    .await
                    .unwrap()
                    .header
                    .index,
            );
        }
        let gateway: Ipv4Addr = "10.242.1.1".parse().unwrap();
        let ownership = install(indices[0], Some(indices[1]), gateway, Some(42))
            .await
            .unwrap();
        let mark = super::super::native_xfrm::mark(42);
        for (protocol, offset) in [([8, 0], 30), ([8, 6], 38)] {
            let mut packet = [0u8; 60];
            packet[12..14].copy_from_slice(&protocol);
            for destination in [gateway, "10.242.1.4".parse().unwrap()] {
                packet[offset..offset + 4].copy_from_slice(&destination.octets());
                let expected = if destination == gateway { 0 } else { 7 };
                assert_eq!(
                    run("nullnet_endpoint_ingress", indices[0], &packet, mark).0,
                    expected
                );
                for bad_mark in [0, mark + 1] {
                    assert_eq!(
                        run("nullnet_endpoint_ingress", indices[0], &packet, bad_mark).0,
                        2
                    );
                }
                assert_eq!(
                    run("nullnet_endpoint_ingress", indices[1], &packet, 0).0,
                    expected
                );
            }
            assert_eq!(
                run("nullnet_endpoint_egress", indices[0], &packet, 0),
                (0, mark)
            );
            assert_eq!(
                run("nullnet_endpoint_egress", indices[1], &packet, 123),
                (0, 123)
            );
        }
        let packet = [0u8; 60];
        ownership.revoke().unwrap();
        for index in &indices {
            for program in ["nullnet_endpoint_ingress", "nullnet_endpoint_egress"] {
                assert_eq!(run(program, *index, &packet, mark).0, 2);
            }
        }
        // An old lease must never delete a replacement interface's configuration.
        let mut map = STATE.get().unwrap().map.lock().unwrap();
        for (index, output, crypto) in [(indices[0], indices[1], mark), (indices[1], indices[0], 0)]
        {
            map.insert(
                index,
                EndpointConfig {
                    gateway: u32::from_ne_bytes(gateway.octets()),
                    output,
                    mark: crypto,
                    padding: 0,
                },
                1,
            )
            .unwrap();
        }
        drop(map);
        ownership.revoke().unwrap();
        drop(ownership);
        let mut map = STATE.get().unwrap().map.lock().unwrap();
        assert!(map.get(&indices[0], 0).is_ok());
        for index in &indices {
            map.remove(index).unwrap();
        }
        drop(map);
        for index in &indices {
            root.handle.link().del(*index).execute().await.unwrap();
        }
        // Publication of the second interface fails; the first entry rolls back.
        root.handle
            .link()
            .add(rtnetlink::LinkDummy::new("nnct_partial").build())
            .execute()
            .await
            .unwrap();
        let index = super::super::netlink::get_link_by_name(&root.handle, "nnct_partial")
            .await
            .unwrap()
            .header
            .index;
        assert!(
            install(index, Some(u32::MAX - 1), gateway, Some(42))
                .await
                .is_err()
        );
        assert!(
            STATE
                .get()
                .unwrap()
                .map
                .lock()
                .unwrap()
                .keys()
                .next()
                .is_none()
        );
        root.handle.link().del(index).execute().await.unwrap();
    }
}
