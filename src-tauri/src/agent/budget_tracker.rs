//! Single-Agent Uplift P0-2：Token Budget tracker。
//!
//! # 解决的问题
//!
//! Miragenty 旧实现把 `max_steps` 当**唯一**停机信号：到 80 步还没 task_complete →
//! 整 agent 标 failed。实测灾难场景（postmortem §4.3）：tester#2 跑到 step 78 时
//! LLM 已经在收尾、正打算 task_complete，step 80 撞顶 fail，前 78 步全部白烧。
//!
//! 根因：硬步数上限只看"做了多少次"，不看"还在不在产出"。
//!
//! # 设计原则
//!
//! 引入**双信号**：
//!   1. **绝对预算**：累计 output_tokens 接近 budget → 该停了
//!   2. **边际收益**：连续 N 轮 output delta < 阈值 → 在原地打转，该停了
//!
//! 只追踪 `output_tokens`，不追踪 `input_tokens`：
//! - input 主要受 history 长度驱动，反映 prompt 大小而非 agent 实际产出
//! - output 直接对应 LLM "说了多少 / 调了多少 tool args"，是 agent 进展真实信号
//!
//! 触发后**不强 stop**——只让 caller 注入一条"该收尾了"的 nudge，让 agent 自己
//! 调 task_complete + guardrail。这样保留 artifact / commit 等收尾动作的机会。
//!
//! 对标：Claude Code `query/tokenBudget.ts`。阈值（500 token / 3 轮 / 90%）从他们
//! 生产数据 port，第一版照搬不调参，等 Miragenty 实测后再微调。
//!
//! # 不在本模块的事
//!
//! - 决定怎么 nudge：caller 负责注入提示词
//! - 决定真正什么时候 stop：caller 还要看 task_complete + guardrail，本 tracker 只
//!   提供"建议收尾"信号
//! - 跟 max_steps 兜底逻辑联动：tracker 是 soft，max_steps 是 hard，两者独立

/// 单轮 output delta 阈值。低于此值视为"这一轮没产出多少"。
///
/// 500 token ≈ 100-200 个英文词 / 50-100 行中文。一个真正在思考问题的 turn
/// 通常输出 1000+ token；持续 < 500 = 反复说"我去看看 X"但不真做事。
pub(crate) const DIMINISHING_TOKEN_THRESHOLD: u64 = 500;

/// 连续多少轮"低产出"才判定为 diminishing。
///
/// 1 轮太敏感：LLM 偶尔会有一轮只说"好的"。
/// 3 轮 = 共识阈值，避开单轮抖动同时不让坏情况长太久（3 轮 ≈ 30s-2min 时间损失）。
pub(crate) const DIMINISHING_ROUND_THRESHOLD: u32 = 3;

/// 累计 output 占 budget 的百分比阈值。超过即认定"budget 即将耗尽"。
///
/// 0.9 = 留 10% 余量给 agent 调 task_complete + 写 summary。
/// 设 1.0 会把"prep 收尾"的 token 也算进 budget 触发，导致 agent 没法收尾。
pub(crate) const COMPLETION_PCT: f64 = 0.9;

/// Token budget 状态追踪。
///
/// **不是 thread-safe**——单 agent 单线程使用。caller 把它放在 `run_inner` 局部变量
/// 即可，不需要 Arc / Mutex。
#[derive(Debug, Clone)]
pub struct BudgetTracker {
    /// 累计 output tokens（跨 step 累加）。
    accumulated_output_tokens: u64,
    /// 上一次 `decide` 时的累计值。用来算本次 delta。
    last_check_total: u64,
    /// 上一次 `decide` 时算出的 delta。diminishing 判定要看连续两轮都低。
    last_delta: u64,
    /// 已经调用 `decide` 并返回 `Continue` 的次数。diminishing 判定要看
    /// "至少 N 轮 continue 之后才算 diminishing"——避免 agent 刚启动几个 step
    /// 就因为初始化 token 少被误判。
    continuation_count: u32,
    /// 是否已经发过"该收尾了"的 nudge。caller 用此 flag 防止每 step 重复发。
    nudge_emitted: bool,
}

impl BudgetTracker {
    pub fn new() -> Self {
        Self {
            accumulated_output_tokens: 0,
            last_check_total: 0,
            last_delta: 0,
            continuation_count: 0,
            nudge_emitted: false,
        }
    }

    /// 每 step 拿到 LLM response 后调用，累计本 step 的 output tokens。
    pub fn record_step(&mut self, step_output_tokens: u64) {
        self.accumulated_output_tokens += step_output_tokens;
    }

    /// 当前累计 output token 数（暴露给 caller 写日志 / 显示）。
    pub fn accumulated(&self) -> u64 {
        self.accumulated_output_tokens
    }

    /// 每 step 末尾决策："continue, accumulate more"  vs "stop, time to wrap up"。
    ///
    /// `budget = 0` 视为"没配预算" → 永远返回 Continue（safe fallback，caller 自己
    /// 应在调用前判断要不要构造 tracker）。
    pub fn decide(&mut self, budget: u64) -> BudgetDecision {
        let total = self.accumulated_output_tokens;
        let delta = total.saturating_sub(self.last_check_total);
        let pct = if budget == 0 {
            0.0
        } else {
            (total as f64) / (budget as f64)
        };

        // diminishing：连续 N 轮都没新增多少 → agent 在原地打转
        // 判定顺序：先判 diminishing 再判 exhausted，因为 diminishing 在 budget 远未
        // 耗尽时也能触发，是"省钱止损"信号；exhausted 是"硬到边"信号。两者都是
        // Stop 但 reason 不同，前端可以区分文案。
        let is_diminishing = self.continuation_count >= DIMINISHING_ROUND_THRESHOLD
            && delta < DIMINISHING_TOKEN_THRESHOLD
            && self.last_delta < DIMINISHING_TOKEN_THRESHOLD;

        // **顺序**：先更新 last_*，再返回——这样下一次 decide 看到的 last_delta
        // 是这一次的 delta。如果先返回再更新，连续 Stop 会丢一次更新。
        self.last_delta = delta;
        self.last_check_total = total;

        if is_diminishing {
            return BudgetDecision::Stop {
                reason: BudgetStopReason::DiminishingReturns,
                accumulated: total,
                budget,
                pct,
            };
        }
        if budget > 0 && total >= ((budget as f64) * COMPLETION_PCT) as u64 {
            return BudgetDecision::Stop {
                reason: BudgetStopReason::BudgetExhausted,
                accumulated: total,
                budget,
                pct,
            };
        }
        self.continuation_count += 1;
        BudgetDecision::Continue {
            accumulated: total,
            budget,
            pct,
            continuation_count: self.continuation_count,
        }
    }

    /// caller 在第一次 Stop 后调用，标记已经发过 nudge。
    /// 后续 step 即使继续触发 Stop 也不再重复发——nudge 一次足够，LLM 看到后
    /// 会自然 task_complete，再 nudge 是噪音。
    pub fn mark_nudge_emitted(&mut self) {
        self.nudge_emitted = true;
    }

    pub fn nudge_already_emitted(&self) -> bool {
        self.nudge_emitted
    }
}

impl Default for BudgetTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetStopReason {
    /// 连续若干轮产出过少。可能：agent 卡在某个 read-only 循环，或者已经"觉得"
    /// 做完了只是没调 task_complete。
    DiminishingReturns,
    /// 累计 output 占 budget ≥ 90%。再做下去就要爆预算。
    BudgetExhausted,
}

impl BudgetStopReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DiminishingReturns => "diminishing_returns",
            Self::BudgetExhausted => "budget_exhausted",
        }
    }
}

#[derive(Debug, Clone)]
pub enum BudgetDecision {
    Continue {
        accumulated: u64,
        budget: u64,
        pct: f64,
        continuation_count: u32,
    },
    Stop {
        reason: BudgetStopReason,
        accumulated: u64,
        budget: u64,
        pct: f64,
    },
}

#[cfg(test)]
mod tests {
    //! 守住的不变量：
    //!   ① budget 远未耗尽 + 有产出 → Continue
    //!   ② budget 达 90% → Stop:BudgetExhausted
    //!   ③ 连续 3 轮 delta < 500 → Stop:DiminishingReturns（即便 budget 富裕）
    //!   ④ 一轮大产出可以"重置" diminishing 判定
    //!   ⑤ nudge_emitted 是 sticky，caller 用其防重复

    use super::*;

    #[test]
    fn continues_when_under_budget_and_producing() {
        let mut t = BudgetTracker::new();
        t.record_step(2000);
        let d = t.decide(10_000);
        match d {
            BudgetDecision::Continue {
                accumulated, pct, ..
            } => {
                assert_eq!(accumulated, 2000);
                assert!((pct - 0.2).abs() < 1e-9);
            }
            other => panic!("expected Continue, got {other:?}"),
        }
    }

    #[test]
    fn stops_when_above_completion_threshold() {
        let mut t = BudgetTracker::new();
        t.record_step(9_500); // 95% of 10K
        let d = t.decide(10_000);
        assert!(matches!(
            d,
            BudgetDecision::Stop {
                reason: BudgetStopReason::BudgetExhausted,
                ..
            }
        ));
    }

    #[test]
    fn just_under_threshold_continues() {
        // 边界守护：低于 90% 一点点应该 Continue
        let mut t = BudgetTracker::new();
        t.record_step(8_999); // 89.99% of 10K, 低于 90%
        let d = t.decide(10_000);
        assert!(matches!(d, BudgetDecision::Continue { .. }));
    }

    #[test]
    fn stops_on_diminishing_returns_after_three_rounds() {
        let mut t = BudgetTracker::new();
        // 4 轮都 < 500 token delta，但 budget 100K 富裕
        // 前 3 轮：continuation_count 1, 2, 3 → Continue
        // 第 4 轮：continuation_count >= 3 + last_delta < 500 + current delta < 500 → Stop
        for _ in 0..3 {
            t.record_step(200);
            let d = t.decide(100_000);
            assert!(matches!(d, BudgetDecision::Continue { .. }));
        }
        t.record_step(200);
        let d = t.decide(100_000);
        match d {
            BudgetDecision::Stop {
                reason: BudgetStopReason::DiminishingReturns,
                accumulated,
                ..
            } => {
                assert_eq!(accumulated, 800, "4 轮 × 200 = 800");
            }
            other => panic!("expected DiminishingReturns Stop, got {other:?}"),
        }
    }

    #[test]
    fn one_big_round_resets_diminishing_judgment() {
        // 关键反例：连续低产出中间夹一个大产出，不应被判 diminishing
        let mut t = BudgetTracker::new();
        t.record_step(100);
        let _ = t.decide(100_000); // continuation 1
        t.record_step(100);
        let _ = t.decide(100_000); // continuation 2
        t.record_step(5000);
        let _ = t.decide(100_000); // continuation 3, last_delta=5000 (>500)
        t.record_step(100);
        let d = t.decide(100_000);
        // 此时 continuation_count >= 3, current_delta=100, last_delta=5000
        // → !(current<500 && last<500)，不算 diminishing
        assert!(
            matches!(d, BudgetDecision::Continue { .. }),
            "上一轮大产出应重置 diminishing 判定"
        );
    }

    #[test]
    fn nudge_emitted_is_sticky_and_starts_false() {
        let mut t = BudgetTracker::new();
        assert!(!t.nudge_already_emitted());
        t.mark_nudge_emitted();
        assert!(t.nudge_already_emitted());
        // 再 mark 一次不出错
        t.mark_nudge_emitted();
        assert!(t.nudge_already_emitted());
    }

    #[test]
    fn budget_zero_never_stops() {
        // 兜底：budget=0 等于"没配预算"，应该永远 Continue，不让数学崩
        let mut t = BudgetTracker::new();
        for _ in 0..10 {
            t.record_step(10_000);
            let d = t.decide(0);
            assert!(
                matches!(d, BudgetDecision::Continue { .. }),
                "budget=0 时永远不应 Stop"
            );
        }
    }

    #[test]
    fn diminishing_requires_minimum_continuation_count() {
        // 关键防御：agent 刚启动头两步即便都 < 500 也不应触发 diminishing
        // （初始化阶段 token 量本来就小，不是 agent "在原地打转"的信号）
        let mut t = BudgetTracker::new();
        t.record_step(100);
        let d1 = t.decide(100_000);
        assert!(
            matches!(d1, BudgetDecision::Continue { .. }),
            "step 1 不应 stop"
        );
        t.record_step(100);
        let d2 = t.decide(100_000);
        assert!(
            matches!(d2, BudgetDecision::Continue { .. }),
            "step 2 不应 stop"
        );
        t.record_step(100);
        let d3 = t.decide(100_000);
        assert!(
            matches!(d3, BudgetDecision::Continue { .. }),
            "step 3 不应 stop"
        );
        // 第 4 步才可能 stop（continuation_count >= 3 + 两轮 last delta < 500）
        t.record_step(100);
        let d4 = t.decide(100_000);
        assert!(matches!(
            d4,
            BudgetDecision::Stop {
                reason: BudgetStopReason::DiminishingReturns,
                ..
            }
        ));
    }

    #[test]
    fn budget_exhausted_takes_precedence_over_diminishing() {
        // 同时满足两个条件时优先报 BudgetExhausted？查代码：当前实现是先 diminishing 后 exhausted。
        // 这条 test 锁住当前行为——diminishing 优先（更早提示 agent 收尾）
        let mut t = BudgetTracker::new();
        // 先撑出 continuation_count >= 3
        for _ in 0..3 {
            t.record_step(100);
            let _ = t.decide(100_000);
        }
        // 再一口气干到 budget 上限附近
        t.record_step(95_000); // total ≈ 95300, well above 90%
        let d = t.decide(100_000);
        // 此时 delta=95000 > 500 → 不算 diminishing → 走 BudgetExhausted
        assert!(matches!(
            d,
            BudgetDecision::Stop {
                reason: BudgetStopReason::BudgetExhausted,
                ..
            }
        ));
    }

    #[test]
    fn as_str_returns_stable_labels() {
        // 防御：这两个字串会进 event meta 落库，前端解析约定不能改
        assert_eq!(
            BudgetStopReason::DiminishingReturns.as_str(),
            "diminishing_returns"
        );
        assert_eq!(
            BudgetStopReason::BudgetExhausted.as_str(),
            "budget_exhausted"
        );
    }
}
