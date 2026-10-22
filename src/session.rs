use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use cookie_store::CookieStore;
use serde::{Serialize, Deserialize};

use crate::identity::Identity;
use crate::error::{Result, ScoutError};

#[derive(Debug)]
pub struct Session {
    pub id: Uuid,
    pub identity: Identity,
    pub cookies: Arc<RwLock<CookieStore>>,
}

impl Session {
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            identity: Identity::generate(),
            cookies: Arc::new(RwLock::new(CookieStore::default())),
        }
    }

    pub fn export_cookies(&self) -> Result<String> {
        let store = self.cookies.read().unwrap();
        let mut writer = Vec::new();
        let mut ser = serde_json::Serializer::new(&mut writer);
        store.serialize(&mut ser).map_err(|e| ScoutError::Session(e.to_string()))?;
        Ok(String::from_utf8(writer).map_err(|e| ScoutError::Session(e.to_string()))?)
    }

    pub fn import_cookies(&self, json: &str) -> Result<()> {
        let mut store = self.cookies.write().unwrap();
        let mut de = serde_json::Deserializer::from_slice(json.as_bytes());
        let new_store = CookieStore::deserialize(&mut de)
            .map_err(|e| ScoutError::Session(e.to_string()))?;
        *store = new_store;
        Ok(())
    }

    pub fn clear_cookies(&self) {
        let mut store = self.cookies.write().unwrap();
        store.clear();
    }
}

/// On-disk sessions live in ~/.webscout/sessions/<name>/{identity,cookies}.json.
fn session_dir(name: &str) -> Result<std::path::PathBuf> {
    // Names become directory names, so keep them path-safe.
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(ScoutError::Session(format!("invalid session name {name:?}")));
    }
    let home = std::env::var("HOME").map_err(|_| ScoutError::Session("HOME not set".into()))?;
    Ok(std::path::Path::new(&home).join(".webscout/sessions").join(name))
}

impl Session {
    /// Load a named session from disk, creating a fresh identity if none exists.
    pub fn load(name: &str) -> Result<Self> {
        let dir = session_dir(name)?;
        let session = Self::new();
        let identity = std::fs::read_to_string(dir.join("identity.json")).ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(session.identity);
        let session = Self { identity, ..session };
        // Unreadable jar = start with no cookies rather than fail the request.
        if let Ok(f) = std::fs::File::open(dir.join("cookies.json")) {
            if let Ok(jar) = cookie_store::serde::json::load_all(std::io::BufReader::new(f)) {
                *session.cookies.write().unwrap() = jar;
            }
        }
        Ok(session)
    }

    pub fn save(&self, name: &str) -> Result<()> {
        let dir = session_dir(name)?;
        let io = |e: std::io::Error| ScoutError::Session(e.to_string());
        std::fs::create_dir_all(&dir).map_err(io)?;
        let identity = serde_json::to_string_pretty(&self.identity).map_err(|e| ScoutError::Session(e.to_string()))?;
        std::fs::write(dir.join("identity.json"), identity).map_err(io)?;
        // Keep session (no-expiry) cookies too: auth cookies pasted from a real browser usually are.
        let mut jar = Vec::new();
        cookie_store::serde::json::save_incl_expired_and_nonpersistent(&self.cookies.read().unwrap(), &mut jar)
            .map_err(|e| ScoutError::Session(e.to_string()))?;
        std::fs::write(dir.join("cookies.json"), jar).map_err(io)
    }

    /// Returns false if the session didn't exist.
    pub fn delete(name: &str) -> Result<bool> {
        match std::fs::remove_dir_all(session_dir(name)?) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(ScoutError::Session(e.to_string())),
        }
    }

    pub fn exists(name: &str) -> bool {
        session_dir(name).is_ok_and(|d| d.join("identity.json").is_file())
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
pub struct SessionStore {
    sessions: tokio::sync::RwLock<HashMap<String, Arc<Session>>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get_or_create(&self, id: Option<&str>) -> Arc<Session> {
        if let Some(id_str) = id {
            let sessions = self.sessions.read().await;
            if let Some(session) = sessions.get(id_str) {
                return Arc::clone(session);
            }
            drop(sessions);

            let mut sessions = self.sessions.write().await;
            let session = Arc::new(Session::new());
            sessions.insert(id_str.to_string(), Arc::clone(&session));
            session
        } else {
            Arc::new(Session::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_cookies_survive_save_load() {
        let name = format!("test-{}", Uuid::new_v4());
        let s = Session::new();
        let url = url::Url::parse("https://ex.com/").unwrap();
        s.cookies.write().unwrap().insert_raw(&cookie_store::Cookie::parse("a=1", &url).unwrap(), &url).unwrap();
        s.save(&name).unwrap();
        let back = Session::load(&name).unwrap();
        Session::delete(&name).unwrap();
        assert_eq!(back.identity.user_agent, s.identity.user_agent);
        assert_eq!(back.cookies.read().unwrap().matches(&url).len(), 1);
        assert!(Session::load("../x").is_err());
    }
}
