use crate::TAP_NAME;
use nullnet_liberror::{ErrorHandler, Location, location};
use std::process::Command;

#[derive(Debug)]
pub(super) enum OvsCommand<'a> {
    DeleteBridge,
    AddBridge,
    DeleteFlows,
    /// Traffic arriving from the trunk (already decrypted by nullnet-client's
    /// userspace forwarder) gets delivered by normal VLAN-aware L2 switching.
    AddTrunkDeliveryFlow,
    /// Traffic arriving from any access port always goes out the trunk,
    /// never directly to another access port. Without this, two access
    /// ports for the same vlan_id that happen to live on the same host's
    /// bridge (i.e. the tunnel's two endpoints are colocated) would be
    /// switched directly by OVS, bypassing the TAP and the encrypting
    /// userspace forwarder entirely.
    AddAccessRedirectFlow,
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
            | OvsCommand::AddTrunkDeliveryFlow
            | OvsCommand::AddAccessRedirectFlow => "ovs-ofctl",
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
            OvsCommand::AddTrunkDeliveryFlow => [
                "add-flow",
                "br0",
                &format!("priority=200,in_port={TAP_NAME},actions=normal"),
            ]
            .iter()
            .map(ToString::to_string)
            .collect(),
            OvsCommand::AddAccessRedirectFlow => [
                "add-flow",
                "br0",
                &format!("priority=100,actions=output:{TAP_NAME}"),
            ]
            .iter()
            .map(ToString::to_string)
            .collect(),
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
