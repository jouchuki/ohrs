//! Bridge configuration types.

use serde::{Deserialize, Serialize};

/// Default session timeout: 24 hours in milliseconds.
pub const DEFAULT_SESSION_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;

/// Type of work item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkType {
    Session,
    Healthcheck,
}

/// Work item metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkData {
    pub r#type: WorkType,
    pub id: String,
}

/// Decoded work secret.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkSecret {
    pub version: u32,
    pub session_ingress_token: String,
    pub api_base_url: String,
}

/// Minimal bridge configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeConfig {
    pub dir: String,
    pub machine_name: String,
    #[serde(default = "default_max_sessions")]
    pub max_sessions: u32,
    #[serde(default)]
    pub verbose: bool,
    #[serde(default = "default_session_timeout")]
    pub session_timeout_ms: u64,
}

fn default_max_sessions() -> u32 {
    1
}

fn default_session_timeout() -> u64 {
    DEFAULT_SESSION_TIMEOUT_MS
}

/// Runtime record of a bridge session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeSessionRecord {
    pub session_id: String,
    pub command: String,
    pub cwd: String,
    pub pid: u32,
    pub status: String,
    pub started_at: f64,
    pub output_path: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_work_type_serde_roundtrip() {
        for wt in [WorkType::Session, WorkType::Healthcheck] {
            let json = serde_json::to_string(&wt).unwrap();
            let deser: WorkType = serde_json::from_str(&json).unwrap();
            assert_eq!(deser, wt);
        }
    }

    #[test]
    fn test_work_data_serde_roundtrip() {
        let data = WorkData {
            r#type: WorkType::Session,
            id: "sess-1".into(),
        };
        let json = serde_json::to_string(&data).unwrap();
        let deser: WorkData = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.id, "sess-1");
    }

    #[test]
    fn test_work_secret_serde_roundtrip() {
        let secret = WorkSecret {
            version: 1,
            session_ingress_token: "token123".into(),
            api_base_url: "https://api.example.com".into(),
        };
        let json = serde_json::to_string(&secret).unwrap();
        let deser: WorkSecret = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.version, 1);
        assert_eq!(deser.session_ingress_token, "token123");
    }

    #[test]
    fn test_bridge_config_serde_roundtrip() {
        let config = BridgeConfig {
            dir: "/tmp/bridge".into(),
            machine_name: "my-machine".into(),
            max_sessions: 5,
            verbose: true,
            session_timeout_ms: 3600000,
        };
        let json = serde_json::to_string(&config).unwrap();
        let deser: BridgeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.max_sessions, 5);
        assert!(deser.verbose);
    }

    #[test]
    fn test_bridge_config_deserialize_defaults() {
        let json = r#"{"dir":"/tmp","machine_name":"m"}"#;
        let config: BridgeConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.max_sessions, 1);
        assert!(!config.verbose);
        assert_eq!(config.session_timeout_ms, DEFAULT_SESSION_TIMEOUT_MS);
    }

    #[test]
    fn test_default_session_timeout_ms() {
        assert_eq!(DEFAULT_SESSION_TIMEOUT_MS, 24 * 60 * 60 * 1000);
    }

    #[test]
    fn test_bridge_session_record_serde_roundtrip() {
        let record = BridgeSessionRecord {
            session_id: "s1".into(),
            command: "cargo test".into(),
            cwd: "/home".into(),
            pid: 1234,
            status: "running".into(),
            started_at: 100.0,
            output_path: "/tmp/out".into(),
        };
        let json = serde_json::to_string(&record).unwrap();
        let deser: BridgeSessionRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.session_id, "s1");
        assert_eq!(deser.pid, 1234);
    }
}
