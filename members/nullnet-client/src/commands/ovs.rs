use crate::TAP_NAME;
use nullnet_liberror::{ErrorHandler, Location, location};
use std::process::Command;

#[derive(Debug)]
pub(super) enum OvsCommand<'a> {
    DeleteBridge,
    AddBridge,
    DeleteFlows,
    /// Fallback for anything not covered by a more specific rule below —
    /// same behavior as OVS's original single default flow. Mainly covers
    /// the brief startup window between an access port being created and
    /// its own redirect flow (below) landing.
    AddDefaultFlow,
    /// Traffic arriving from the trunk (already decrypted by nullnet-client's
    /// userspace forwarder) gets delivered by normal VLAN-aware L2 switching.
    AddTrunkDeliveryFlow,
    /// One rule per access port, installed alongside it: redirect this
    /// port's traffic to the trunk instead of letting OVS switch it
    /// directly to another local access port (which would bypass the TAP
    /// and the encrypting userspace forwarder entirely when a tunnel's two
    /// endpoints happen to be colocated on this host). `output:<port>` is a
    /// raw action — unlike `actions=normal`, it does *not* re-add the
    /// 802.1Q tag that access ports carry only internally, so this
    /// explicitly pushes the tag back on first: without that, packets would
    /// arrive at nullnet-client's TAP already stripped of their VLAN tag
    /// and get silently dropped as malformed.
    AddAccessRedirectFlow(&'a str, u16),
    /// Removes exactly the rule `AddAccessRedirectFlow` installed for this
    /// port, so a torn-down tunnel doesn't leave a stale flow entry that
    /// could wrongly match a future, unrelated port reusing the same
    /// OVS port number.
    DeleteAccessRedirectFlow(&'a str),
    AddTrunkPort,
    AddAccessPort(&'a str, u16),
}

impl OvsCommand<'_> {
    pub(super) fn execute(&self) {
        let init_t = std::time::Instant::now();
        let _ = Command::new(self.program())
            .args(self.args())
            .spawn()
            .map(|mut c| c.wait())
            .handle_err(location!());
        println!(
            "Executed command {:?} in {} ms",
            self,
            init_t.elapsed().as_millis()
        );
    }

    fn program(&self) -> &str {
        match self {
            OvsCommand::AddBridge
            | OvsCommand::DeleteBridge
            | OvsCommand::AddAccessPort(_, _)
            | OvsCommand::AddTrunkPort => "ovs-vsctl",
            OvsCommand::DeleteFlows
            | OvsCommand::AddDefaultFlow
            | OvsCommand::AddTrunkDeliveryFlow
            | OvsCommand::AddAccessRedirectFlow(_, _)
            | OvsCommand::DeleteAccessRedirectFlow(_) => "ovs-ofctl",
        }
    }

    fn args(&self) -> Vec<String> {
        match self {
            OvsCommand::AddBridge => ["add-br", "br0"].iter().map(ToString::to_string).collect(),
            OvsCommand::DeleteBridge => ["del-br", "br0"].iter().map(ToString::to_string).collect(),
            OvsCommand::DeleteFlows => ["del-flows", "br0"]
                .iter()
                .map(ToString::to_string)
                .collect(),
            OvsCommand::AddDefaultFlow => ["add-flow", "br0", "priority=0,actions=normal"]
                .iter()
                .map(ToString::to_string)
                .collect(),
            OvsCommand::AddTrunkDeliveryFlow => [
                "add-flow",
                "br0",
                &format!("priority=200,in_port={TAP_NAME},actions=normal"),
            ]
            .iter()
            .map(ToString::to_string)
            .collect(),
            OvsCommand::AddAccessRedirectFlow(dev, vlan) => [
                "add-flow",
                "br0",
                &format!(
                    "priority=150,in_port={dev},actions=push_vlan:0x8100,mod_vlan_vid:{vlan},output:{TAP_NAME}"
                ),
            ]
            .iter()
            .map(ToString::to_string)
            .collect(),
            OvsCommand::DeleteAccessRedirectFlow(dev) => {
                ["del-flows", "br0", &format!("in_port={dev}")]
                    .iter()
                    .map(ToString::to_string)
                    .collect()
            }
            OvsCommand::AddTrunkPort => ["add-port", "br0", TAP_NAME]
                .iter()
                .map(ToString::to_string)
                .collect(),
            OvsCommand::AddAccessPort(dev, vlan) => {
                ["add-port", "br0", dev, &format!("tag={vlan}")]
                    .iter()
                    .map(ToString::to_string)
                    .collect()
            }
        }
    }
}
