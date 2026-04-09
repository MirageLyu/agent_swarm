use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SlotStatus {
    Unfilled,
    Tentative,
    Confirmed,
    Skipped,
}

impl SlotStatus {
    pub fn score(&self) -> f64 {
        match self {
            SlotStatus::Unfilled => 0.0,
            SlotStatus::Tentative => 0.5,
            SlotStatus::Confirmed => 1.0,
            SlotStatus::Skipped => 0.8,
        }
    }

    fn rank(&self) -> u8 {
        match self {
            SlotStatus::Unfilled => 0,
            SlotStatus::Tentative => 1,
            SlotStatus::Skipped => 2,
            SlotStatus::Confirmed => 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotState {
    pub status: SlotStatus,
    pub value: Option<String>,
    pub confirmed_at_round: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationPhase {
    Exploring,
    Narrowing,
    Confirming,
    ReadyToSign,
}

impl ConversationPhase {
    pub fn label(&self) -> &'static str {
        match self {
            ConversationPhase::Exploring => "exploring",
            ConversationPhase::Narrowing => "narrowing",
            ConversationPhase::Confirming => "confirming",
            ConversationPhase::ReadyToSign => "ready_to_sign",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "narrowing" => ConversationPhase::Narrowing,
            "confirming" => ConversationPhase::Confirming,
            "ready_to_sign" => ConversationPhase::ReadyToSign,
            _ => ConversationPhase::Exploring,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotDefinition {
    pub id: String,
    pub weight: f64,
    pub section: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflightBeliefState {
    pub round: u32,
    pub max_rounds: u32,
    pub slots: HashMap<String, SlotState>,
    pub convergence_score: f64,
    pub phase: ConversationPhase,
}

impl Default for PreflightBeliefState {
    fn default() -> Self {
        Self::new()
    }
}

impl PreflightBeliefState {
    pub fn new() -> Self {
        let mut slots = HashMap::new();
        for def in default_slot_definitions() {
            slots.insert(
                def.id.clone(),
                SlotState {
                    status: SlotStatus::Unfilled,
                    value: None,
                    confirmed_at_round: None,
                },
            );
        }
        Self {
            round: 0,
            max_rounds: 12,
            slots,
            convergence_score: 0.0,
            phase: ConversationPhase::Exploring,
        }
    }

    pub fn compute_convergence_score(&mut self) {
        let defs = default_slot_definitions();
        let mut score = 0.0;
        for def in &defs {
            if let Some(slot) = self.slots.get(&def.id) {
                score += def.weight * slot.status.score();
            }
        }
        self.convergence_score = (score * 1000.0).round() / 1000.0;
    }

    /// Update phase based on convergence_score. Phase never regresses.
    pub fn update_phase(&mut self) {
        let new_phase = if self.convergence_score >= 0.85 {
            ConversationPhase::ReadyToSign
        } else if self.convergence_score >= 0.65 {
            ConversationPhase::Confirming
        } else if self.convergence_score >= 0.3 {
            ConversationPhase::Narrowing
        } else {
            ConversationPhase::Exploring
        };

        if new_phase > self.phase {
            self.phase = new_phase;
        }
    }

    /// Update a slot's status. Status never regresses (e.g., Confirmed won't become Tentative).
    pub fn update_slot(
        &mut self,
        slot_id: &str,
        status: SlotStatus,
        value: Option<String>,
        round: u32,
    ) {
        if let Some(slot) = self.slots.get_mut(slot_id) {
            if status.rank() > slot.status.rank() {
                slot.status = status;
                slot.confirmed_at_round = Some(round);
            }
            if value.is_some() {
                slot.value = value;
            }
        }
    }

    pub fn increment_round(&mut self) {
        self.round += 1;
    }
}

pub fn default_slot_definitions() -> Vec<SlotDefinition> {
    vec![
        SlotDefinition { id: "primary_goal".into(), weight: 0.20, section: "scope".into() },
        SlotDefinition { id: "target_users".into(), weight: 0.10, section: "scope".into() },
        SlotDefinition { id: "key_features".into(), weight: 0.15, section: "scope".into() },
        SlotDefinition { id: "tech_constraints".into(), weight: 0.10, section: "constraints".into() },
        SlotDefinition { id: "performance_targets".into(), weight: 0.05, section: "constraints".into() },
        SlotDefinition { id: "security_requirements".into(), weight: 0.08, section: "constraints".into() },
        SlotDefinition { id: "integration_points".into(), weight: 0.07, section: "scope".into() },
        SlotDefinition { id: "out_of_scope".into(), weight: 0.10, section: "exclusions".into() },
        SlotDefinition { id: "risk_assumptions".into(), weight: 0.08, section: "assumptions".into() },
        SlotDefinition { id: "timeline_budget".into(), weight: 0.07, section: "constraints".into() },
    ]
}

/// Map an add_contract_item call to the most relevant slot based on section + keywords.
pub fn map_contract_item_to_slot(section: &str, item: &str) -> Option<&'static str> {
    let lower = item.to_lowercase();

    match section {
        "scope" => {
            if contains_any(&lower, &["目标", "核心", "主要", "goal", "objective", "primary", "purpose"]) {
                Some("primary_goal")
            } else if contains_any(&lower, &["用户", "角色", "受众", "user", "audience", "target user", "persona"]) {
                Some("target_users")
            } else if contains_any(&lower, &["集成", "接入", "对接", "integration", "api", "interface", "third-party"]) {
                Some("integration_points")
            } else if contains_any(&lower, &["功能", "特性", "模块", "feature", "capability", "implement"]) {
                Some("key_features")
            } else {
                Some("key_features")
            }
        }
        "constraints" => {
            if contains_any(&lower, &["技术", "框架", "语言", "tech", "framework", "stack", "language"]) {
                Some("tech_constraints")
            } else if contains_any(&lower, &["性能", "速度", "延迟", "performance", "latency", "throughput"]) {
                Some("performance_targets")
            } else if contains_any(&lower, &["安全", "权限", "认证", "security", "auth", "encryption"]) {
                Some("security_requirements")
            } else if contains_any(&lower, &["时间", "预算", "期限", "timeline", "budget", "deadline", "schedule"]) {
                Some("timeline_budget")
            } else {
                Some("tech_constraints")
            }
        }
        "exclusions" => Some("out_of_scope"),
        "assumptions" => Some("risk_assumptions"),
        _ => None,
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ut_10_2_1a_default_init() {
        let bs = PreflightBeliefState::new();
        assert_eq!(bs.slots.len(), 10);
        assert_eq!(bs.convergence_score, 0.0);
        assert_eq!(bs.phase, ConversationPhase::Exploring);
        for slot in bs.slots.values() {
            assert_eq!(slot.status, SlotStatus::Unfilled);
        }
    }

    #[test]
    fn ut_10_2_2a_all_unfilled() {
        let mut bs = PreflightBeliefState::new();
        bs.compute_convergence_score();
        assert!((bs.convergence_score - 0.0).abs() < 0.001);
    }

    #[test]
    fn ut_10_2_2b_all_confirmed() {
        let mut bs = PreflightBeliefState::new();
        for slot in bs.slots.values_mut() {
            slot.status = SlotStatus::Confirmed;
        }
        bs.compute_convergence_score();
        assert!((bs.convergence_score - 1.0).abs() < 0.001);
    }

    #[test]
    fn ut_10_2_2c_mixed_state() {
        let mut bs = PreflightBeliefState::new();
        bs.slots.get_mut("primary_goal").unwrap().status = SlotStatus::Confirmed;
        bs.slots.get_mut("target_users").unwrap().status = SlotStatus::Tentative;
        bs.compute_convergence_score();
        let expected = 0.2 * 1.0 + 0.1 * 0.5; // 0.25
        assert!((bs.convergence_score - expected).abs() < 0.001);
    }

    #[test]
    fn ut_10_2_2d_with_skipped() {
        let mut bs = PreflightBeliefState::new();
        bs.slots.get_mut("primary_goal").unwrap().status = SlotStatus::Confirmed;
        bs.slots.get_mut("out_of_scope").unwrap().status = SlotStatus::Skipped;
        bs.compute_convergence_score();
        let expected = 0.2 * 1.0 + 0.1 * 0.8; // 0.28
        assert!((bs.convergence_score - expected).abs() < 0.001);
    }

    #[test]
    fn ut_10_2_3a_initial_phase() {
        let bs = PreflightBeliefState::new();
        assert_eq!(bs.phase, ConversationPhase::Exploring);
    }

    #[test]
    fn ut_10_2_3b_score_driven_narrowing() {
        let mut bs = PreflightBeliefState::new();
        bs.convergence_score = 0.35;
        bs.update_phase();
        assert_eq!(bs.phase, ConversationPhase::Narrowing);
    }

    #[test]
    fn ut_10_2_3f_high_score_ready() {
        let mut bs = PreflightBeliefState::new();
        bs.convergence_score = 0.90;
        bs.update_phase();
        assert_eq!(bs.phase, ConversationPhase::ReadyToSign);
    }

    #[test]
    fn ut_10_2_3g_phase_no_regress() {
        let mut bs = PreflightBeliefState::new();
        bs.convergence_score = 0.35;
        bs.update_phase();
        assert_eq!(bs.phase, ConversationPhase::Narrowing);

        bs.convergence_score = 0.25;
        bs.update_phase();
        assert_eq!(bs.phase, ConversationPhase::Narrowing);
    }

    #[test]
    fn ut_10_2_4a_scope_primary_goal() {
        let slot = map_contract_item_to_slot("scope", "实现用户登录系统的核心目标");
        assert_eq!(slot, Some("primary_goal"));
    }

    #[test]
    fn ut_10_2_4b_scope_target_users() {
        let slot = map_contract_item_to_slot("scope", "面向B端企业用户");
        assert_eq!(slot, Some("target_users"));
    }

    #[test]
    fn ut_10_2_4c_constraints_tech() {
        let slot = map_contract_item_to_slot("constraints", "使用React+Node.js技术栈");
        assert_eq!(slot, Some("tech_constraints"));
    }

    #[test]
    fn ut_10_2_4d_exclusions() {
        let slot = map_contract_item_to_slot("exclusions", "不包含支付功能");
        assert_eq!(slot, Some("out_of_scope"));
    }

    #[test]
    fn ut_10_2_4f_slot_no_regress() {
        let mut bs = PreflightBeliefState::new();
        bs.update_slot("primary_goal", SlotStatus::Confirmed, Some("goal".into()), 1);
        assert_eq!(bs.slots["primary_goal"].status, SlotStatus::Confirmed);

        bs.update_slot("primary_goal", SlotStatus::Tentative, Some("updated".into()), 2);
        assert_eq!(bs.slots["primary_goal"].status, SlotStatus::Confirmed);
    }

    #[test]
    fn weight_sum_is_one() {
        let defs = default_slot_definitions();
        let sum: f64 = defs.iter().map(|d| d.weight).sum();
        assert!((sum - 1.0).abs() < 0.001);
    }
}
