//! Reconcile the host-wide guard for the shared metadata VXLAN.
use nullnet_grpc_lib::{
    NullnetGrpcInterface,
    nullnet_grpc::{AgentEvent, AgentVxlanEnvironmentFailed, agent_event::Event},
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::process::Command;

static READY: AtomicBool = AtomicBool::new(false);

pub(super) fn ready() -> bool {
    READY.load(Ordering::Acquire)
}

async fn iptables(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("iptables")
        .args(["-w", "2"])
        .args(args)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| error.to_string())
}

async fn checked(args: &[&str]) -> Result<(), String> {
    let output = iptables(args).await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

async fn reconcile() -> Result<bool, String> {
    let mut changed = false;
    for (chain, direction) in [("INPUT", "in"), ("OUTPUT", "out")] {
        let rule = [
            "-p",
            "udp",
            "--dport",
            "4791",
            "-m",
            "policy",
            "--dir",
            direction,
            "--pol",
            "none",
            "-m",
            "comment",
            "--comment",
            "nullnet-policy-ciphertext",
            "-j",
            "DROP",
        ];
        let present = iptables(&[&["-C", chain][..], &rule].concat())
            .await?
            .status
            .success();
        if !present {
            checked(&[&["-I", chain, "1"][..], &rule].concat()).await?;
            changed = true;
        }
    }
    let routed = [
        "-p",
        "udp",
        "--dport",
        "4791",
        "-m",
        "mark",
        "--mark",
        "0x80000000/0x80000000",
        "-m",
        "comment",
        "--comment",
        "nullnet-policy-xfrm-route",
        "-j",
        "ACCEPT",
    ];
    let present = iptables(&[&["-C", "OUTPUT"][..], &routed].concat())
        .await?
        .status
        .success();
    if present {
        checked(&[&["-D", "OUTPUT"][..], &routed].concat()).await?;
        changed = true;
    }
    Ok(changed)
}

pub(crate) async fn start(grpc: NullnetGrpcInterface) -> Result<(), String> {
    reconcile().await?;
    READY.store(true, Ordering::Release);
    tokio::spawn(async move {
        let mut failed = false;
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            match reconcile().await {
                Ok(changed) => {
                    READY.store(true, Ordering::Release);
                    if changed || failed {
                        eprintln!("Shared VXLAN plaintext guards restored");
                    }
                    failed = false;
                }
                Err(error) => {
                    READY.store(false, Ordering::Release);
                    if !failed {
                        let _ = grpc
                            .report_event(AgentEvent {
                                event: Some(Event::VxlanEnvironmentFailed(
                                    AgentVxlanEnvironmentFailed {
                                        error_message: error,
                                    },
                                )),
                            })
                            .await;
                    }
                    failed = true;
                }
            }
        }
    });
    Ok(())
}

#[cfg(test)]
pub(super) async fn initialize_test() -> Result<(), String> {
    reconcile().await?;
    READY.store(true, Ordering::Release);
    Ok(())
}
