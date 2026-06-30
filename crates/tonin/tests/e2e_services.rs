//! Smoke tests for services started by `tonin run --with-deps` in CI.
//!
//! Prerequisites: users-service, orders-service, and products-service must
//! already be running (the test-e2e.yml workflow starts them via
//! `tonin run --service orders-service --with-deps --wait-healthy --background`
//! before invoking this suite).

mod e2e {
    use std::net::TcpStream;
    use std::time::Duration;

    fn assert_reachable(name: &str, port: u16) {
        TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().unwrap(),
            Duration::from_secs(5),
        )
        .unwrap_or_else(|e| panic!("{name} not reachable on 127.0.0.1:{port}: {e}"));
    }

    #[test]
    #[ignore] // requires users-service running locally; see test-e2e.yml
    fn users_service_reachable() {
        assert_reachable("users-service", 50051);
    }

    #[test]
    #[ignore] // requires orders-service running locally; see test-e2e.yml
    fn orders_service_reachable() {
        assert_reachable("orders-service", 50052);
    }

    #[test]
    #[ignore] // requires products-service running locally; see test-e2e.yml
    fn products_service_reachable() {
        assert_reachable("products-service", 50053);
    }
}
