use std::time::Duration;

/// Upper bound for Docker CLI calls. The daemon normally responds in well
/// under 100 ms; five seconds leaves headroom without freezing a worker.
const SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(5);

/// Run `docker` and turn spawn, timeout, and non-zero-exit failures into errors.
pub(crate) async fn run(args: &[&str], label: &str) -> Result<std::process::Output, String> {
    run_command("docker", args, label, SUBPROCESS_TIMEOUT).await
}

async fn run_command(
    program: &str,
    args: &[&str],
    label: &str,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let mut command = tokio::process::Command::new(program);
    command.args(args).kill_on_drop(true);
    let output = match tokio::time::timeout(timeout, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => return Err(format!("{label}: {error}")),
        Err(_) => return Err(format!("{label}: timed out after {timeout:?}")),
    };
    if !output.status.success() {
        return Err(format!(
            "{label} exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn command_spawn_failure_is_an_error() {
        let error = run_command(
            "/definitely/not/a/real/binary",
            &[],
            "docker ps",
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();

        assert!(error.starts_with("docker ps:"));
    }

    #[tokio::test]
    async fn command_failure_is_an_error() {
        let error = run_command(
            "/bin/sh",
            &["-c", "printf failure >&2; exit 7"],
            "docker ps",
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();

        assert!(error.contains("exited exit status: 7"));
        assert!(error.contains("failure"));
    }

    #[tokio::test]
    async fn command_hang_is_bounded() {
        let error = run_command(
            "/bin/sleep",
            &["30"],
            "docker ps",
            Duration::from_millis(20),
        )
        .await
        .unwrap_err();

        assert!(error.contains("timed out"));
    }
}
