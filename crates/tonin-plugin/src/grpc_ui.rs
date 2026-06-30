//! gRPC UI backend for launching grpcui against running services.
//!
//! Provides a simple wrapper around the grpcui binary to enable interactive
//! gRPC service inspection via a web UI.

use std::process::Command;

/// Backend for launching grpcui against a gRPC service.
pub struct GrpcUiBackend;

impl GrpcUiBackend {
    /// Launch grpcui against a running gRPC service.
    ///
    /// Assumes `grpcui` is available in $PATH. Connects via plaintext (no TLS)
    /// to the target service and prints the connection URL for the user.
    ///
    /// # Arguments
    ///
    /// * `service_name` - The name of the service (for logging)
    /// * `service_port` - The port on which the gRPC service is listening
    ///
    /// # Errors
    ///
    /// Returns an error if `grpcui` is not found or fails to launch.
    pub fn launch_grpcui(
        service_name: &str,
        service_port: u16,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Check if grpcui is available
        let check = Command::new("which").arg("grpcui").status();

        if check.is_err() {
            return Err(
                "grpcui not found in $PATH. Install it with: go install github.com/fullstorydev/grpcui/cmd/grpcui@latest".into()
            );
        }

        let target = format!("localhost:{}", service_port);

        println!(
            "Launching grpcui for service '{}' on {}...",
            service_name, target
        );
        println!("grpcui will open in your browser at: http://localhost:8080");
        println!();

        let output = Command::new("grpcui")
            .arg("-plaintext")
            .arg(&target)
            .spawn();

        match output {
            Ok(mut child) => {
                // Wait for the process (this blocks until grpcui exits)
                let status = child.wait()?;
                if !status.success() {
                    return Err("grpcui exited with non-zero status".into());
                }
                Ok(())
            }
            Err(e) => Err(format!("Failed to launch grpcui: {}", e).into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grpc_ui_backend_creation() {
        let _backend = GrpcUiBackend;
        // Smoke test: just ensure we can create the backend
    }
}
