use std::collections::HashMap;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Global registry that maps running agent IDs to their CancellationTokens.
pub struct AgentRegistry {
    tokens: Mutex<HashMap<String, CancellationToken>>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self {
            tokens: Mutex::new(HashMap::new()),
        }
    }

    pub fn register(&self, agent_id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        self.tokens
            .lock()
            .unwrap()
            .insert(agent_id.to_string(), token.clone());
        token
    }

    pub fn cancel(&self, agent_id: &str) -> bool {
        if let Some(token) = self.tokens.lock().unwrap().get(agent_id) {
            token.cancel();
            true
        } else {
            false
        }
    }

    pub fn remove(&self, agent_id: &str) {
        self.tokens.lock().unwrap().remove(agent_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_registered_agent() {
        let reg = AgentRegistry::new();
        let token = reg.register("a1");
        assert!(!token.is_cancelled());
        assert!(reg.cancel("a1"));
        assert!(token.is_cancelled());
    }

    #[test]
    fn cancel_is_idempotent() {
        let reg = AgentRegistry::new();
        let token = reg.register("a1");
        assert!(reg.cancel("a1"));
        assert!(reg.cancel("a1"));
        assert!(token.is_cancelled());
    }

    #[test]
    fn cancel_unknown_agent_is_harmless() {
        let reg = AgentRegistry::new();
        assert!(!reg.cancel("nonexistent"));
    }

    #[test]
    fn remove_cleans_up() {
        let reg = AgentRegistry::new();
        let _token = reg.register("a1");
        reg.remove("a1");
        assert!(!reg.cancel("a1"));
    }
}
