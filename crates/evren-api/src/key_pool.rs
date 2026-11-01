//! API anahtar havuzu (`api_key_pool.py`): yapışkan geçerli anahtar,
//! soğuma (429) ve devre dışı bırakma (kota/yetki) durumları.

use std::time::{Duration, Instant};

pub const DEFAULT_COOLDOWN_SECONDS: f64 = 60.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    Ready,
    Cooling,
    Disabled,
}

#[derive(Debug, Clone)]
pub struct PooledKey {
    pub key: String,
    pub state: KeyState,
    cooldown_until: Option<Instant>,
    pub failure_count: u32,
    pub last_error: Option<String>,
}

impl PooledKey {
    fn new(key: String) -> Self {
        PooledKey {
            key,
            state: KeyState::Ready,
            cooldown_until: None,
            failure_count: 0,
            last_error: None,
        }
    }

    pub fn masked(&self) -> String {
        mask_key(&self.key)
    }

    pub fn is_available(&self, now: Instant) -> bool {
        match self.state {
            KeyState::Disabled => false,
            KeyState::Cooling => self.cooldown_until.map_or(true, |t| t <= now),
            KeyState::Ready => true,
        }
    }

    /// Soğumanın bitmesine kalan saniye (hazır ya da devre dışı → 0).
    pub fn next_available_in(&self, now: Instant) -> f64 {
        match (self.state, self.cooldown_until) {
            (KeyState::Cooling, Some(t)) if t > now => (t - now).as_secs_f64(),
            _ => 0.0,
        }
    }
}

/// `mask_key`: boş → "(boş)"; kısa → ilk 4 + son 2; uzun → ilk 10 + son 4.
pub fn mask_key(key: &str) -> String {
    if key.is_empty() {
        return "(boş)".to_owned();
    }
    let chars: Vec<char> = key.chars().collect();
    let n = chars.len();
    if n <= 12 {
        let head: String = chars.iter().take(4).collect();
        let tail: String = chars.iter().skip(n.saturating_sub(2)).collect();
        format!("{head}...{tail}")
    } else {
        let head: String = chars.iter().take(10).collect();
        let tail: String = chars.iter().skip(n - 4).collect();
        format!("{head}...{tail}")
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StateCounts {
    pub ready: usize,
    pub cooling: usize,
    pub disabled: usize,
}

#[derive(Debug, Clone)]
pub struct ApiKeyPool {
    keys: Vec<PooledKey>,
    current: usize,
    cooldown: Duration,
}

impl ApiKeyPool {
    pub fn new(keys: Vec<String>) -> Self {
        Self::with_cooldown(keys, DEFAULT_COOLDOWN_SECONDS)
    }

    pub fn with_cooldown(keys: Vec<String>, cooldown_seconds: f64) -> Self {
        let mut seen: Vec<String> = Vec::new();
        for k in keys {
            let k = k.trim().to_owned();
            if !k.is_empty() && !seen.contains(&k) {
                seen.push(k);
            }
        }
        ApiKeyPool {
            keys: seen.into_iter().map(PooledKey::new).collect(),
            current: 0,
            cooldown: Duration::from_secs_f64(cooldown_seconds.max(0.0)),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn keys(&self) -> &[PooledKey] {
        &self.keys
    }

    pub fn key_values(&self) -> Vec<String> {
        self.keys.iter().map(|k| k.key.clone()).collect()
    }

    pub fn current_key(&self) -> Option<&str> {
        self.keys.get(self.current).map(|k| k.key.as_str())
    }

    pub fn has_available(&self) -> bool {
        let now = Instant::now();
        self.keys.iter().any(|k| k.is_available(now))
    }

    /// En erken hazır olacak anahtara kalan saniye (hazır varsa 0).
    pub fn next_available_secs(&self) -> f64 {
        let now = Instant::now();
        self.keys
            .iter()
            .map(|k| k.next_available_in(now))
            .filter(|w| *w > 0.0)
            .fold(None, |acc: Option<f64>, w| {
                Some(acc.map_or(w, |a| a.min(w)))
            })
            .unwrap_or(0.0)
    }

    pub fn state_counts(&self) -> StateCounts {
        let now = Instant::now();
        let mut c = StateCounts::default();
        for k in &self.keys {
            match k.state {
                KeyState::Disabled => c.disabled += 1,
                KeyState::Cooling if !k.is_available(now) => c.cooling += 1,
                _ => c.ready += 1,
            }
        }
        c
    }

    fn index_of(&self, key: &str) -> Option<usize> {
        self.keys.iter().position(|k| k.key == key)
    }

    fn first_available_from(&self, start: usize, now: Instant) -> Option<usize> {
        let n = self.keys.len();
        (0..n)
            .map(|i| (start + i) % n)
            .find(|&i| self.keys[i].is_available(now))
    }

    /// Yapışkan geçerli anahtar; kullanılamıyorsa sıradaki hazır anahtara geçer.
    pub fn acquire(&mut self) -> Option<String> {
        if self.keys.is_empty() {
            return None;
        }
        let now = Instant::now();
        let idx = self.first_available_from(self.current, now)?;
        self.current = idx;
        let pk = &mut self.keys[idx];
        if pk.state == KeyState::Cooling {
            pk.state = KeyState::Ready;
            pk.cooldown_until = None;
        }
        Some(pk.key.clone())
    }

    /// `/keys next`: sıradaki hazır anahtara elle geçer.
    pub fn rotate(&mut self) -> Option<String> {
        if self.keys.is_empty() {
            return None;
        }
        let now = Instant::now();
        let start = (self.current + 1) % self.keys.len();
        let idx = self.first_available_from(start, now)?;
        self.current = idx;
        Some(self.keys[idx].key.clone())
    }

    pub fn report_success(&mut self, key: &str) {
        if let Some(i) = self.index_of(key) {
            let pk = &mut self.keys[i];
            if pk.state != KeyState::Disabled {
                pk.state = KeyState::Ready;
                pk.cooldown_until = None;
                pk.last_error = None;
            }
        }
    }

    pub fn report_rate_limited(&mut self, key: &str, retry_after: Option<f64>) {
        let cooldown = self.cooldown;
        if let Some(i) = self.index_of(key) {
            let pk = &mut self.keys[i];
            pk.failure_count += 1;
            pk.state = KeyState::Cooling;
            let wait = retry_after
                .map(|s| Duration::from_secs_f64(s.max(0.0)))
                .unwrap_or(cooldown);
            pk.cooldown_until = Some(Instant::now() + wait);
            pk.last_error = Some("rate_limited".to_owned());
        }
    }

    pub fn report_disabled(&mut self, key: &str, reason: &str) {
        if let Some(i) = self.index_of(key) {
            let pk = &mut self.keys[i];
            pk.failure_count += 1;
            pk.state = KeyState::Disabled;
            pk.last_error = Some(reason.to_owned());
        }
    }

    pub fn clear_cooldown(&mut self, key: &str) {
        if let Some(i) = self.index_of(key) {
            let pk = &mut self.keys[i];
            if pk.state == KeyState::Cooling {
                pk.state = KeyState::Ready;
                pk.cooldown_until = None;
            }
        }
    }

    pub fn clear_all_cooldowns(&mut self) {
        for pk in &mut self.keys {
            if pk.state == KeyState::Cooling {
                pk.state = KeyState::Ready;
                pk.cooldown_until = None;
            }
        }
    }

    pub fn reset_states(&mut self) {
        for pk in &mut self.keys {
            pk.state = KeyState::Ready;
            pk.cooldown_until = None;
            pk.last_error = None;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn mask() {
        assert_eq!(mask_key(""), "(boş)");
        assert_eq!(mask_key("abcdefgh"), "abcd...gh");
        assert_eq!(mask_key("evren_llm_ABCDEFGHIJKL"), "evren_llm_...IJKL");
    }

    #[test]
    fn sticky_and_failover() {
        let mut p = ApiKeyPool::new(vec!["a".into(), "a".into(), "b".into()]);
        assert_eq!(p.len(), 2);
        assert_eq!(p.acquire().as_deref(), Some("a"));
        assert_eq!(p.acquire().as_deref(), Some("a"));
        p.report_rate_limited("a", Some(60.0));
        assert_eq!(
            p.state_counts(),
            StateCounts {
                ready: 1,
                cooling: 1,
                disabled: 0
            }
        );
        assert_eq!(p.acquire().as_deref(), Some("b"));
        p.report_disabled("b", "quota_exhausted");
        assert!(!p.has_available());
        assert!(p.next_available_secs() > 0.0);
        assert_eq!(p.acquire(), None);
        p.clear_all_cooldowns();
        assert_eq!(p.acquire().as_deref(), Some("a"));
        p.report_rate_limited("a", Some(0.0));
        assert!(p.has_available());
    }
}
