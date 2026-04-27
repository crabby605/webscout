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
