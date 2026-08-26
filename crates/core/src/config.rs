use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{OrchestratorError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineOs {
    WindowsWsl,
    Linux,
    #[default]
    Unknown,
}

fn default_port() -> u16 {
    22
}
fn default_session() -> String {
    "main".to_string()
}
fn default_mcp_port() -> u16 {
    8321
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    pub name: String,
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    #[serde(default)]
    pub os: MachineOs,
    pub ssh_user: String,
    #[serde(default = "default_port")]
    pub ssh_port: u16,
    pub ssh_key: PathBuf,
    #[serde(default = "default_session")]
    pub default_session: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub onboarding_done: bool,
    #[serde(default = "default_mcp_port")]
    pub mcp_port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_auth_token: Option<String>,
    #[serde(default)]
    pub machine: Vec<Machine>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            onboarding_done: false,
            mcp_port: default_mcp_port(),
            mcp_auth_token: None,
            machine: Vec::new(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .map_err(|e| OrchestratorError::ConfigError(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(OrchestratorError::Io(e.to_string())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| OrchestratorError::ConfigError(e.to_string()))?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn find(&self, name: &str) -> Option<&Machine> {
        self.machine.iter().find(|m| m.name == name)
    }

    pub fn upsert(&mut self, m: Machine) {
        match self.machine.iter_mut().find(|x| x.name == m.name) {
            Some(slot) => *slot = m,
            None => self.machine.push(m),
        }
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.machine.len();
        self.machine.retain(|m| m.name != name);
        self.machine.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Machine {
        Machine {
            name: "w1".into(),
            host: "192.168.1.50".into(),
            mac: Some("AA:BB:CC:DD:EE:FF".into()),
            os: MachineOs::Linux,
            ssh_user: "julio".into(),
            ssh_port: 22,
            ssh_key: PathBuf::from("/home/julio/.ssh/id_ed25519"),
            default_session: "main".into(),
        }
    }

    #[test]
    fn load_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load(&dir.path().join("machines.toml")).unwrap();
        assert_eq!(cfg, Config::default());
        assert!(!cfg.onboarding_done);
        assert_eq!(cfg.mcp_port, 8321);
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machines.toml");
        let mut cfg = Config::default();
        cfg.onboarding_done = true;
        cfg.upsert(sample());
        cfg.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded, cfg);
        assert!(
            !path.with_extension("toml.tmp").exists(),
            "tmp file must be renamed away"
        );
    }

    #[test]
    fn save_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("deep").join("machines.toml");
        Config::default().save(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn load_applies_defaults_for_optional_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machines.toml");
        std::fs::write(
            &path,
            r#"
[[machine]]
name = "w1"
host = "10.0.0.1"
ssh_user = "u"
ssh_key = "/k"
"#,
        )
        .unwrap();
        let cfg = Config::load(&path).unwrap();
        let m = &cfg.machine[0];
        assert_eq!(m.ssh_port, 22);
        assert_eq!(m.default_session, "main");
        assert_eq!(m.os, MachineOs::Unknown);
        assert_eq!(m.mac, None);
    }

    #[test]
    fn load_invalid_toml_is_config_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machines.toml");
        std::fs::write(&path, "this = [is not toml").unwrap();
        let err = Config::load(&path).unwrap_err();
        assert!(matches!(err, OrchestratorError::ConfigError(_)));
    }

    #[test]
    fn upsert_replaces_by_name_and_remove_works() {
        let mut cfg = Config::default();
        cfg.upsert(sample());
        let mut changed = sample();
        changed.host = "10.0.0.9".into();
        cfg.upsert(changed.clone());
        assert_eq!(cfg.machine.len(), 1);
        assert_eq!(cfg.find("w1"), Some(&changed));
        assert!(cfg.remove("w1"));
        assert!(!cfg.remove("w1"));
        assert_eq!(cfg.find("w1"), None);
    }

    #[test]
    fn os_serializes_snake_case() {
        let s = toml::to_string(&sample()).unwrap();
        assert!(s.contains("os = \"linux\""));
    }
}
