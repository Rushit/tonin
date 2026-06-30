use anyhow::Result;
use std::process::Command;

/// Arguments for streaming logs from a running service.
#[derive(Debug, Clone)]
pub struct LogsValues {
    /// Service name (e.g. "users-service")
    pub service: String,
    /// Whether to stream continuously (follow)
    pub follow: bool,
    /// Number of tail lines (default: 10)
    pub tail: usize,
    /// Whether to include timestamps
    pub timestamp: bool,
}

/// Arguments for port-forwarding to a service.
#[derive(Debug, Clone)]
pub struct PortForwardValues {
    /// Service name (e.g. "orders-service")
    pub service: String,
    /// Local port to bind to
    pub local_port: u16,
    /// Remote port to forward to
    pub remote_port: u16,
}

/// Pluggable observation backend for logs and port-forwarding.
/// Default: [`DockerComposeBackend`] queries running containers.
/// Swap to [`DryRunBackend`] for testing or `--dry-run`.
pub trait ObserveBackend: Send + Sync + std::fmt::Debug {
    /// Stream logs from a running service.
    /// Returns Ok on successful start (streams until Ctrl+C).
    fn logs(&self, values: &LogsValues) -> Result<()>;

    /// Establish port-forward from local to service's remote port.
    /// Returns Ok on successful tunnel setup (runs until Ctrl+C).
    fn port_forward(&self, values: &PortForwardValues) -> Result<()>;
}

/// Default backend: queries logs via Docker Compose containers.
#[derive(Debug)]
pub struct DockerComposeBackend;

impl ObserveBackend for DockerComposeBackend {
    fn logs(&self, values: &LogsValues) -> Result<()> {
        // Container name follows docker-compose convention: <service>-1 or <service>
        let container_name = &values.service;

        let mut cmd = Command::new("docker");
        cmd.args(["logs", container_name]);

        if values.follow {
            cmd.arg("--follow");
        }

        // Format: "docker logs --tail <N> --timestamps <container>"
        if values.tail > 0 {
            cmd.arg("--tail");
            cmd.arg(values.tail.to_string());
        }

        if values.timestamp {
            cmd.arg("--timestamps");
        }

        let status = cmd
            .status()
            .map_err(|e| anyhow::anyhow!("docker logs failed for {}: {e}", values.service))?;

        anyhow::ensure!(
            status.success(),
            "docker logs exited with status: {} for service {}",
            status.code().unwrap_or(-1),
            values.service
        );

        Ok(())
    }

    fn port_forward(&self, values: &PortForwardValues) -> Result<()> {
        // Use docker port command to get the container's exposed port,
        // then fall back to basic socat-like behavior via docker exec/socat
        // For now, use docker's network inspection to create a tunnel.

        // Get container inspect info to find the running container
        let inspect_output = Command::new("docker")
            .args(["inspect", &values.service])
            .output()
            .map_err(|e| anyhow::anyhow!("failed to inspect container {}: {e}", values.service))?;

        if !inspect_output.status.success() {
            anyhow::bail!("container '{}' not found or not running", values.service);
        }

        // For Docker, we can use: docker run --rm -it --net container:<container> \
        //   -p <local>:<remote> alpine/socat tcp-listen:<remote>,reuseaddr,fork tcp-connect:localhost:<remote>
        // But simpler approach: just use docker port or attempt direct docker exec tunnel

        println!(
            "▶ port-forward {} (local:{}→remote:{})",
            values.service, values.local_port, values.remote_port
        );
        println!(
            "  run: nc -l localhost {} | nc localhost {}",
            values.local_port, values.remote_port
        );

        // Simple approach: spawn a listener and forward
        // For Docker Compose, containers are accessible via <service>:<port>
        let listener = std::net::TcpListener::bind(("127.0.0.1", values.local_port))
            .map_err(|e| anyhow::anyhow!("failed to bind to port {}: {e}", values.local_port))?;

        println!(
            "✓ listening on localhost:{}, forwarding to {}:{}",
            values.local_port, values.service, values.remote_port
        );
        println!("  (press Ctrl+C to exit)");

        // Accept connections and forward
        for stream in listener.incoming() {
            match stream {
                Ok(local_stream) => {
                    let remote_addr = format!("{}:{}", values.service, values.remote_port);
                    // Attempt to connect to remote
                    match std::net::TcpStream::connect(remote_addr.as_str()) {
                        Ok(remote_stream) => {
                            // Simple bidirectional forwarding
                            let mut local_read = local_stream.try_clone()?;
                            let mut local_write = local_stream;
                            let mut remote_read = remote_stream.try_clone()?;
                            let mut remote_write = remote_stream;

                            std::thread::spawn(move || {
                                let _ = std::io::copy(&mut local_read, &mut remote_write);
                            });
                            std::thread::spawn(move || {
                                let _ = std::io::copy(&mut remote_read, &mut local_write);
                            });
                        }
                        Err(e) => {
                            eprintln!(
                                "failed to connect to {}:{}: {}",
                                values.service, values.remote_port, e
                            );
                        }
                    }
                }
                Err(e) => {
                    eprintln!("error accepting connection: {e}");
                }
            }
        }

        Ok(())
    }
}

/// Stub backend for Kubernetes (k3d/kubectl). Placeholder for future phases.
#[derive(Debug)]
pub struct KubectlBackend {
    pub namespace: String,
}

impl KubectlBackend {
    pub fn new(namespace: String) -> Self {
        Self { namespace }
    }
}

impl ObserveBackend for KubectlBackend {
    fn logs(&self, values: &LogsValues) -> Result<()> {
        // Stub: kubectl logs <pod> -n <namespace>
        let mut cmd = Command::new("kubectl");
        cmd.args(["logs", &values.service]);
        cmd.args(["-n", &self.namespace]);

        if values.follow {
            cmd.arg("--follow");
        }

        if values.tail > 0 {
            cmd.arg("--tail");
            cmd.arg(values.tail.to_string());
        }

        if values.timestamp {
            cmd.arg("--timestamps");
        }

        let status = cmd
            .status()
            .map_err(|e| anyhow::anyhow!("kubectl logs failed for {}: {e}", values.service))?;

        anyhow::ensure!(
            status.success(),
            "kubectl logs exited with status: {} for service {}",
            status.code().unwrap_or(-1),
            values.service
        );

        Ok(())
    }

    fn port_forward(&self, values: &PortForwardValues) -> Result<()> {
        // Stub: kubectl port-forward pod/<pod> <local>:<remote> -n <namespace>
        let port_spec = format!("{}:{}", values.local_port, values.remote_port);
        let pod_ref = format!("pod/{}", values.service);
        let status = Command::new("kubectl")
            .args(["port-forward", &pod_ref, &port_spec, "-n", &self.namespace])
            .status()
            .map_err(|e| anyhow::anyhow!("kubectl port-forward failed: {e}"))?;

        anyhow::ensure!(
            status.success(),
            "kubectl port-forward exited with status: {}",
            status.code().unwrap_or(-1)
        );

        Ok(())
    }
}

/// No-op backend for `--dry-run` and unit tests. Prints what it would do.
#[derive(Debug)]
pub struct DryRunBackend;

impl ObserveBackend for DryRunBackend {
    fn logs(&self, values: &LogsValues) -> Result<()> {
        let follow_str = if values.follow { "--follow" } else { "" };
        let tail_str = if values.tail > 0 {
            format!("--tail {}", values.tail)
        } else {
            String::new()
        };
        let ts_str = if values.timestamp { "--timestamps" } else { "" };

        println!(
            "DRY: logs {} {} {} {} (follow={})",
            values.service, tail_str, ts_str, follow_str, values.follow
        );
        Ok(())
    }

    fn port_forward(&self, values: &PortForwardValues) -> Result<()> {
        println!(
            "DRY: port-forward {} (local:{}→remote:{})",
            values.service, values.local_port, values.remote_port
        );
        println!(
            "  would listen on 127.0.0.1:{} and forward to {}:{}",
            values.local_port, values.service, values.remote_port
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dry_run_logs() -> Result<()> {
        let backend = DryRunBackend;
        let values = LogsValues {
            service: "users-service".to_string(),
            follow: true,
            tail: 10,
            timestamp: true,
        };
        backend.logs(&values)?;
        Ok(())
    }

    #[test]
    fn test_dry_run_logs_no_follow() -> Result<()> {
        let backend = DryRunBackend;
        let values = LogsValues {
            service: "orders-service".to_string(),
            follow: false,
            tail: 50,
            timestamp: false,
        };
        backend.logs(&values)?;
        Ok(())
    }

    #[test]
    fn test_dry_run_port_forward() -> Result<()> {
        let backend = DryRunBackend;
        let values = PortForwardValues {
            service: "products-service".to_string(),
            local_port: 8001,
            remote_port: 50051,
        };
        backend.port_forward(&values)?;
        Ok(())
    }

    #[test]
    fn test_logs_values_construction() {
        let values = LogsValues {
            service: "api-service".to_string(),
            follow: true,
            tail: 100,
            timestamp: true,
        };
        assert_eq!(values.service, "api-service");
        assert!(values.follow);
        assert_eq!(values.tail, 100);
        assert!(values.timestamp);
    }

    #[test]
    fn test_port_forward_values_construction() {
        let values = PortForwardValues {
            service: "db-service".to_string(),
            local_port: 5432,
            remote_port: 5432,
        };
        assert_eq!(values.service, "db-service");
        assert_eq!(values.local_port, 5432);
        assert_eq!(values.remote_port, 5432);
    }

    #[test]
    fn test_kubernetes_backend_creation() {
        let backend = KubectlBackend::new("default".to_string());
        assert_eq!(backend.namespace, "default");
    }

    #[test]
    fn test_docker_compose_backend_creation() {
        let backend = DockerComposeBackend;
        // Just verify it exists and can be created
        let _ = format!("{:?}", backend);
    }
}
