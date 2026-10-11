//! 受管 Edge 的链路场景（集成测试实例）。场景本体见 `scenarios/node_link.rs`。
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

#[path = "scenarios/node_link.rs"]
mod scenarios;

#[tokio::test]
async fn reports_state_traffic_and_obeys_pushes() {
    scenarios::reports_state_traffic_and_obeys_pushes().await;
}

#[tokio::test]
async fn reconnect_resyncs_and_the_data_plane_keeps_running() {
    scenarios::reconnect_resyncs_and_the_data_plane_keeps_running().await;
}

#[tokio::test]
async fn rejection_and_silence_are_handled() {
    scenarios::rejection_and_silence_are_handled().await;
}

#[tokio::test]
async fn device_enrollment_is_relayed() {
    scenarios::device_enrollment_is_relayed().await;
}

#[tokio::test]
async fn protocol_failures_only_cause_retries() {
    scenarios::protocol_failures_only_cause_retries().await;
}

#[tokio::test]
async fn cache_write_failures_do_not_stop_the_node() {
    scenarios::cache_write_failures_do_not_stop_the_node().await;
}

#[tokio::test]
async fn stale_counters_are_swept_while_the_controller_is_away() {
    scenarios::stale_counters_are_swept_while_the_controller_is_away().await;
}
