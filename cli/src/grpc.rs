use std::process::{Child, Command, Stdio};

use crate::scope::Scope;

pub(crate) fn grpc_call(
    scope: &Scope,
    service: &str,
    port: u16,
    method: &str,
    payload: &str,
) -> Result<String, String> {
    let namespace = scope.release_name()?;
    let mut pf = start_port_forward(&namespace, service, port)?;
    let result = call_grpcurl(port, method, payload);
    stop_port_forward(&mut pf);
    result
}

fn start_port_forward(namespace: &str, service: &str, port: u16) -> Result<Child, String> {
    let child = Command::new("kubectl")
        .args([
            "port-forward",
            &format!("svc/{service}"),
            &format!("{port}:{port}"),
            "-n",
            namespace,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to start port-forward: {e}"))?;

    std::thread::sleep(std::time::Duration::from_secs(2));

    Ok(child)
}

fn call_grpcurl(port: u16, method: &str, payload: &str) -> Result<String, String> {
    let output = Command::new("grpcurl")
        .args([
            "-plaintext",
            "-max-time",
            "15",
            "-d",
            payload,
            &format!("localhost:{port}"),
            method,
        ])
        .output()
        .map_err(|e| format!("failed to run grpcurl: {e}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("grpcurl failed: {stderr}"))
    }
}

fn stop_port_forward(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    #[test]
    fn port_forward_args() {
        let args = [
            "port-forward",
            "svc/tightbeam-controller",
            "9090:9090",
            "-n",
            "my-project",
        ];
        assert_eq!(args[0], "port-forward");
        assert_eq!(args[1], "svc/tightbeam-controller");
        assert!(args[2].contains("9090"));
    }

    #[test]
    fn grpcurl_args() {
        let method = "tightbeam.v1.TightbeamController/ChannelStream";
        let payload = r#"{"register":{"channel_type":"test"}}"#;
        let addr = format!("localhost:{}", 9090);
        assert_eq!(addr, "localhost:9090");
        assert!(method.contains("TightbeamController"));
        assert!(payload.contains("register"));
    }
}
