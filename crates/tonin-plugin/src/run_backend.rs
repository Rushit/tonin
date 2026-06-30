use anyhow::Result;
use std::collections::HashMap;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Track a running service process.
#[derive(Debug, Clone)]
pub struct RunningService {
    pub name: String,
    pub pid: u32,
    pub started_at: SystemTime,
}

/// Arguments for running a single service.
#[derive(Debug, Clone)]
pub struct RunValues {
    /// Service name (e.g. "users-service")
    pub service: String,
    /// Service directory where Cargo.toml lives
    pub service_dir: String,
    /// Environment variables to pass to cargo run
    pub env_vars: HashMap<String, String>,
}

/// Pluggable run backend. Default: [`CargoRunBackend`] uses `cargo run` with watch.
/// Swap to [`DryRunBackend`] for testing or previewing without actually running.
pub trait RunBackend: Send + Sync + std::fmt::Debug {
    /// Start a service locally using cargo run.
    /// Returns Ok on successful startup, Err if startup fails.
    fn start_service(&self, values: &RunValues) -> Result<()>;

    /// Stop a running service gracefully.
    fn stop_service(&self, service_name: &str) -> Result<()>;

    /// Watch for source changes and restart on file changes.
    /// This is a blocking call — the caller should handle Ctrl+C to exit.
    fn watch_service(&self, service_name: &str, service_dir: &str) -> Result<()>;
}

/// Default backend: shells out to `cargo run` with cargo-watch for hot reload.
pub struct CargoRunBackend {
    /// Map of service_name -> running process Child for stopping later
    running_processes: Arc<Mutex<HashMap<String, Child>>>,
    /// Track running services with PID and startup time
    running_services: Arc<Mutex<Vec<RunningService>>>,
}

impl std::fmt::Debug for CargoRunBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CargoRunBackend").finish()
    }
}

impl CargoRunBackend {
    pub fn new() -> Self {
        Self {
            running_processes: Arc::new(Mutex::new(HashMap::new())),
            running_services: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Get the list of currently running services with their PIDs and startup times.
    pub fn running_services(&self) -> Vec<RunningService> {
        self.running_services.lock().unwrap().clone()
    }
}

impl Default for CargoRunBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl RunBackend for CargoRunBackend {
    fn start_service(&self, values: &RunValues) -> Result<()> {
        println!("▶ starting {} in {}", values.service, values.service_dir);

        let mut cmd = Command::new("cargo");
        cmd.arg("run").current_dir(&values.service_dir);

        // Set environment variables
        for (key, val) in &values.env_vars {
            cmd.env(key, val);
        }

        let child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("failed to start cargo run for {}: {e}", values.service)
        })?;

        let pid = child.id();
        let service_name = values.service.clone();

        // Track the running service
        let running_svc = RunningService {
            name: service_name.clone(),
            pid,
            started_at: SystemTime::now(),
        };
        self.running_services.lock().unwrap().push(running_svc);

        let mut procs = self.running_processes.lock().unwrap();
        procs.insert(service_name, child);

        Ok(())
    }

    fn stop_service(&self, service_name: &str) -> Result<()> {
        let mut procs = self.running_processes.lock().unwrap();
        if let Some(mut child) = procs.remove(service_name) {
            println!("⏹ stopping {}", service_name);

            let pid = child.id() as i32;

            // Send SIGTERM for graceful shutdown
            #[cfg(unix)]
            {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }

            #[cfg(not(unix))]
            {
                // Fallback to kill on non-Unix platforms
                child.kill().ok();
            }

            // Wait for graceful shutdown (up to 5 seconds)
            let start = std::time::Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break, // Process exited gracefully
                    Ok(None) => {
                        if start.elapsed().as_secs() >= 5 {
                            // Timeout reached, force kill
                            println!("⚠ {} did not exit gracefully, force killing", service_name);
                            child.kill().ok();
                            child.wait().ok();
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    Err(e) => {
                        eprintln!("error waiting for {} to exit: {}", service_name, e);
                        break;
                    }
                }
            }
        }

        // Remove from running_services tracking
        let mut services = self.running_services.lock().unwrap();
        services.retain(|s| s.name != service_name);

        Ok(())
    }

    fn watch_service(&self, service_name: &str, service_dir: &str) -> Result<()> {
        println!("▶ watching {} for changes in {}", service_name, service_dir);

        // Try to use cargo-watch if available; otherwise fall back to cargo run
        let has_cargo_watch = Command::new("cargo")
            .arg("watch")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        let mut cmd = if has_cargo_watch {
            let mut c = Command::new("cargo");
            c.arg("watch").arg("-x").arg("run").current_dir(service_dir);
            c
        } else {
            let mut c = Command::new("cargo");
            c.arg("run").current_dir(service_dir);
            c
        };

        let status = cmd
            .status()
            .map_err(|e| anyhow::anyhow!("failed to run cargo for {}: {e}", service_name))?;

        anyhow::ensure!(
            status.success(),
            "cargo run/watch exited with status: {status}"
        );

        Ok(())
    }
}

/// No-op backend for `--dry-run` and unit tests. Prints what it would do.
#[derive(Debug)]
pub struct DryRunBackend;

impl RunBackend for DryRunBackend {
    fn start_service(&self, values: &RunValues) -> Result<()> {
        println!(
            "DRY: cargo run in {} (service={})",
            values.service_dir, values.service
        );
        if !values.env_vars.is_empty() {
            println!("  with env vars:");
            for (k, v) in &values.env_vars {
                println!("    {}={}", k, v);
            }
        }
        Ok(())
    }

    fn stop_service(&self, service_name: &str) -> Result<()> {
        println!("DRY: stop service {}", service_name);
        Ok(())
    }

    fn watch_service(&self, service_name: &str, service_dir: &str) -> Result<()> {
        println!(
            "DRY: watch {} in {} with cargo watch/run",
            service_name, service_dir
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dry_run_start_service() -> Result<()> {
        let backend = DryRunBackend;
        let values = RunValues {
            service: "users-service".to_string(),
            service_dir: "./users-service".to_string(),
            env_vars: HashMap::new(),
        };
        backend.start_service(&values)?;
        Ok(())
    }

    #[test]
    fn test_dry_run_start_service_with_env() -> Result<()> {
        let backend = DryRunBackend;
        let mut env_vars = HashMap::new();
        env_vars.insert(
            "DATABASE_URL".to_string(),
            "postgres://localhost".to_string(),
        );
        env_vars.insert("PORT".to_string(), "50051".to_string());

        let values = RunValues {
            service: "users-service".to_string(),
            service_dir: "./users-service".to_string(),
            env_vars,
        };
        backend.start_service(&values)?;
        Ok(())
    }

    #[test]
    fn test_dry_run_stop_service() -> Result<()> {
        let backend = DryRunBackend;
        backend.stop_service("users-service")?;
        Ok(())
    }

    #[test]
    fn test_dry_run_watch_service() -> Result<()> {
        let backend = DryRunBackend;
        backend.watch_service("users-service", "./users-service")?;
        Ok(())
    }

    #[test]
    fn test_run_values_construction() {
        let mut env_vars = HashMap::new();
        env_vars.insert(
            "DATABASE_URL".to_string(),
            "postgres://localhost".to_string(),
        );

        let values = RunValues {
            service: "orders-service".to_string(),
            service_dir: "./orders-service".to_string(),
            env_vars,
        };

        assert_eq!(values.service, "orders-service");
        assert_eq!(values.service_dir, "./orders-service");
        assert_eq!(values.env_vars.len(), 1);
    }

    #[test]
    fn test_cargo_run_backend_creation() {
        let backend = CargoRunBackend::new();
        let procs = backend.running_processes.lock().unwrap();
        assert_eq!(procs.len(), 0);
    }

    #[test]
    fn test_running_services_initially_empty() {
        let backend = CargoRunBackend::new();
        let services = backend.running_services();
        assert_eq!(services.len(), 0);
    }

    #[test]
    fn test_running_service_struct_fields() {
        let svc = RunningService {
            name: "test-service".to_string(),
            pid: 12345,
            started_at: SystemTime::now(),
        };

        assert_eq!(svc.name, "test-service");
        assert_eq!(svc.pid, 12345);
        // started_at should be very recent
        let elapsed = svc.started_at.elapsed().unwrap();
        assert!(elapsed.as_secs() < 1);
    }

    #[test]
    fn test_run_values_with_multiple_env_vars() -> Result<()> {
        let mut env_vars = HashMap::new();
        env_vars.insert(
            "DATABASE_URL".to_string(),
            "postgres://localhost".to_string(),
        );
        env_vars.insert("PORT".to_string(), "50051".to_string());
        env_vars.insert("LOG_LEVEL".to_string(), "debug".to_string());

        let values = RunValues {
            service: "multi-env-service".to_string(),
            service_dir: "./services/multi-env-service".to_string(),
            env_vars,
        };

        assert_eq!(values.service, "multi-env-service");
        assert_eq!(values.env_vars.len(), 3);
        assert_eq!(values.env_vars.get("LOG_LEVEL").unwrap(), "debug");
        Ok(())
    }
}
