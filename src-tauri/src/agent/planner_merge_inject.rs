//! Explicit Merge Node v1 —— planner 后处理：为多 parent 汇合点自动插入 merge 节点。
//!
//! ## 算法（v1.1 修正）
//!
//! 对每个 `depends_on.len() >= 2` 的 task X，插入**恰好 1 个** merge 节点：
//!
//! ```text
//! N=2:  A,B           → M(A,B)        → X
//! N=3:  A,B,C         → M(A,B,C)      → X
//! N=4:  A,B,C,D       → M(A,B,C,D)    → X
//! N=8:  P1..P8        → M(P1..P8)     → X    (1 merge node, not 7)
//! ```
//!
//! N parents → **1 个** merge node（不是 N-1）。X 最终只依赖该 merge node。
//! merge node 的 `depends_on` 与 `merge_parents` 都等于原 parents 全集。
//!
//! ## 为什么不再用二叉 reduction tree（设计动机修正）
//!
//! v1 初稿用了二叉 reduction tree（N parents → N-1 merge nodes，深度 log2 N），
//! 模仿 MPI_Reduce / parallel sort 的"分层合并"。**这是误判**：
//!
//! 1. **没有并行收益**：reduction tree 的核心卖点是并行（worker 同时合并各对），
//!    但 merge agent 必须等 parents 全部 completed 才能跑，单 agent 内部又是串行
//!    LLM session——分层只会让端到端**时延变长**（log2 N 个串行 LLM session
//!    vs 单 session）。
//! 2. **DAG 视觉膨胀**：N=8 → 7 个紫色 ◇ 节点，UI 拓扑爆炸。
//! 3. **Token 总成本更高**：每个 merge agent 都有 system prompt + 工具表
//!    overhead，N-1 个 agent 重复这部分；单 agent 一次性看完通常更省。
//! 4. **调试反而更难**：用户要在 N-1 个 sub-merge timeline 之间跳转，而不是
//!    在一个 timeline 里读完整推理。
//! 5. **失败语义没差**：leaf merge 失败 → 上层全白做，重试要从 leaf 重启；
//!    单 merge 失败 → 直接重试该 merge。重试代价一样。
//!
//! 二叉树**唯一**有边际价值的场景：N 极大（比如 N≥10）时单 merge 的 prompt
//! 超出 token budget。这种情况在真实 DAG 中极罕见（多数汇合 N≤4），未来如果
//! 实测有需要可以加 fallback：N <= threshold 用单 merge，N > threshold 才退化
//! 为 reduction tree。
//!
//! ## 为什么不动 consumes_artifacts
//!
//! 下游 X 原本 `consumes_artifacts = ["P1.foo"]`，P1 是其中一个 parent。
//! 注入 merge 节点后 X.depends_on 改成 [M]，但 P1 仍在 X 的**传递依赖闭包**
//! 内（X → M → P1），所以 `planner_state::validate_consumes_artifacts`
//! 仍然通过——不需要重写 X 的 consumes_artifacts。
//!
//! ## 顺序敏感性
//!
//! merge node 的 `depends_on` / `merge_parents` 按 X 原 `depends_on` 出现顺序
//! 保持稳定（不按完成时间，避免运行时拓扑因调度顺序漂移）。LLM prompt 中
//! parent 段落也按此顺序渲染。
//!
//! ## 接入点
//!
//! `parse_and_validate` 中：validate → inject → revalidate。Inject 后必须重新
//! 校验 DAG（新增节点 + 改边可能形成环——理论上算法保证无环，但双保险）。

use crate::agent::planner::{NodeKind, PlannerTask};

/// 注入选项；当前 v1 只暴露一个开关（避免硬切换：默认 false 保持旧行为）。
#[derive(Debug, Clone, Copy, Default)]
pub struct InjectOptions {
    /// 是否启用注入；false 时函数 no-op，所有 `Work` 节点不变。
    pub enabled: bool,
}

/// 对 `tasks` 原地注入 merge 节点。返回新增的 merge node 数量。
///
/// 行为：
/// - 跳过 `kind == Merge` 的任务（避免对已注入的 plan 二次处理，幂等性）
/// - 跳过 `depends_on.len() < 2` 的任务（单 parent / 根节点不需要 merge）
/// - 每个多 parent 任务产生**恰好 1 个** merge node
/// - 新增的 merge node 追加到 `tasks` 末尾（不影响原有任务顺序）
///
/// 如果 `opts.enabled == false`，直接返回 0，不做任何改动。
pub fn inject_merge_nodes(tasks: &mut Vec<PlannerTask>, opts: InjectOptions) -> usize {
    if !opts.enabled {
        return 0;
    }

    // 先收集需要注入的 task index + 原 parents，避免边遍历边改动 tasks。
    let plan: Vec<(usize, Vec<String>)> = tasks
        .iter()
        .enumerate()
        .filter_map(|(idx, t)| {
            if t.kind == NodeKind::Merge {
                None
            } else if t.depends_on.len() >= 2 {
                Some((idx, t.depends_on.clone()))
            } else {
                None
            }
        })
        .collect();

    if plan.is_empty() {
        return 0;
    }

    let mut new_nodes: Vec<PlannerTask> = Vec::new();

    for (task_idx, parents) in plan {
        let downstream_id = tasks[task_idx].id.clone();
        let downstream_title = tasks[task_idx].title.clone();
        let parent_titles: Vec<String> = parents
            .iter()
            .map(|pid| {
                tasks
                    .iter()
                    .find(|t| &t.id == pid)
                    .map(|t| t.title.clone())
                    .unwrap_or_else(|| pid.clone())
            })
            .collect();

        let merge_id = format!("merge-{downstream_id}");
        let merge_title = build_merge_title(&parent_titles, &downstream_title);
        let merge_desc = build_merge_description(&parents, &downstream_id, &downstream_title);

        let merge_task = PlannerTask {
            id: merge_id.clone(),
            title: merge_title,
            description: merge_desc,
            complexity: "low".to_string(),
            depends_on: parents.clone(),
            expected_output: Some(
                "A merged worktree where conflicts (if any) are resolved with intent \
                 preserved from all parents, and `verify_command` (if configured) passes \
                 with exit code 0."
                    .to_string(),
            ),
            role: None,
            additional_skills: Vec::new(),
            produces_artifacts: Vec::new(),
            consumes_artifacts: Vec::new(),
            file_scope_hints: Default::default(),
            kind: NodeKind::Merge,
            merge_parents: parents,
        };

        new_nodes.push(merge_task);
        tasks[task_idx].depends_on = vec![merge_id];
    }

    let added = new_nodes.len();
    tasks.extend(new_nodes);
    added
}

/// 构造 merge node title：N≤3 时列出全部 parent，N≥4 时省略中间显示首末 + 计数。
fn build_merge_title(parent_titles: &[String], downstream_title: &str) -> String {
    let _ = downstream_title;
    match parent_titles.len() {
        0 | 1 => unreachable!("caller filters out N<2"),
        2 => format!("Merge: {} + {}", parent_titles[0], parent_titles[1]),
        3 => format!(
            "Merge: {} + {} + {}",
            parent_titles[0], parent_titles[1], parent_titles[2]
        ),
        n => format!(
            "Merge: {} … {} ({} parents)",
            parent_titles[0],
            parent_titles[n - 1],
            n
        ),
    }
}

/// 构造 merge node description：列出所有上游 task id（不截断；上游通常 ≤8 个）。
fn build_merge_description(parents: &[String], downstream_id: &str, downstream_title: &str) -> String {
    let parents_list = parents
        .iter()
        .map(|p| format!("`{p}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Reconcile worktrees from {n} upstream task(s) ({list}) into a single coherent base so \
         that downstream task `{downstream_id}` ({downstream_title}) can build on top of it. \
         Resolve any cross-parent conflicts preserving intent from every parent, then verify.",
        n = parents.len(),
        list = parents_list,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::planner::{NodeKind, PlannerTask};

    fn work(id: &str, deps: &[&str]) -> PlannerTask {
        PlannerTask {
            id: id.into(),
            title: format!("Task {id}"),
            description: format!("desc {id}"),
            complexity: "medium".into(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            expected_output: None,
            role: None,
            additional_skills: vec![],
            produces_artifacts: vec![],
            consumes_artifacts: vec![],
            file_scope_hints: Default::default(),
            kind: NodeKind::Work,
            merge_parents: vec![],
        }
    }

    #[test]
    fn disabled_is_noop() {
        let mut tasks = vec![work("A", &[]), work("B", &[]), work("X", &["A", "B"])];
        let added = inject_merge_nodes(&mut tasks, InjectOptions { enabled: false });
        assert_eq!(added, 0);
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[2].depends_on, vec!["A", "B"]);
    }

    #[test]
    fn single_parent_no_merge() {
        let mut tasks = vec![work("A", &[]), work("X", &["A"])];
        let added = inject_merge_nodes(&mut tasks, InjectOptions { enabled: true });
        assert_eq!(added, 0);
        assert_eq!(tasks[1].depends_on, vec!["A"]);
    }

    #[test]
    fn diamond_two_parents_one_merge() {
        let mut tasks = vec![work("A", &[]), work("B", &[]), work("X", &["A", "B"])];
        let added = inject_merge_nodes(&mut tasks, InjectOptions { enabled: true });
        assert_eq!(added, 1);
        assert_eq!(tasks.len(), 4);

        let merge_id = &tasks[3].id;
        assert_eq!(merge_id, "merge-X");
        assert_eq!(tasks[3].kind, NodeKind::Merge);
        assert_eq!(tasks[3].depends_on, vec!["A".to_string(), "B".to_string()]);
        assert_eq!(tasks[3].merge_parents, vec!["A".to_string(), "B".to_string()]);
        assert_eq!(tasks[2].depends_on, vec![merge_id.clone()]);
    }

    #[test]
    fn four_parents_single_merge_node() {
        // v1.1 修正：N parents → 1 merge node（不再是 N-1）
        let mut tasks = vec![
            work("A", &[]),
            work("B", &[]),
            work("C", &[]),
            work("D", &[]),
            work("X", &["A", "B", "C", "D"]),
        ];
        let added = inject_merge_nodes(&mut tasks, InjectOptions { enabled: true });
        assert_eq!(added, 1, "4 parents → 1 merge node (not 3)");
        assert_eq!(tasks.len(), 6);

        let x = tasks.iter().find(|t| t.id == "X").unwrap();
        assert_eq!(x.depends_on.len(), 1);
        assert_eq!(x.depends_on[0], "merge-X");

        let merge = tasks.iter().find(|t| t.id == "merge-X").unwrap();
        assert_eq!(merge.kind, NodeKind::Merge);
        assert_eq!(merge.depends_on, vec!["A", "B", "C", "D"]);
        assert_eq!(merge.merge_parents, vec!["A", "B", "C", "D"]);
    }

    #[test]
    fn eight_parents_still_single_merge_node() {
        // 重点回归：N=8 不再生成 7 个 merge（用户提出的 DAG 视觉膨胀问题）
        let parent_ids: Vec<String> = (0..8).map(|i| format!("P{i}")).collect();
        let parent_refs: Vec<&str> = parent_ids.iter().map(|s| s.as_str()).collect();

        let mut tasks: Vec<PlannerTask> =
            parent_ids.iter().map(|p| work(p, &[])).collect();
        tasks.push(work("X", &parent_refs));

        let added = inject_merge_nodes(&mut tasks, InjectOptions { enabled: true });
        assert_eq!(added, 1, "N=8 must yield exactly 1 merge node, got {added}");

        let merge = tasks.iter().find(|t| t.kind == NodeKind::Merge).unwrap();
        assert_eq!(merge.depends_on.len(), 8);
        assert_eq!(merge.merge_parents.len(), 8);
        // 顺序保持稳定
        for (i, p) in parent_ids.iter().enumerate() {
            assert_eq!(&merge.depends_on[i], p);
            assert_eq!(&merge.merge_parents[i], p);
        }
    }

    #[test]
    fn five_parents_keeps_order() {
        let mut tasks = vec![
            work("A", &[]),
            work("B", &[]),
            work("C", &[]),
            work("D", &[]),
            work("E", &[]),
            work("X", &["A", "B", "C", "D", "E"]),
        ];
        let added = inject_merge_nodes(&mut tasks, InjectOptions { enabled: true });
        assert_eq!(added, 1);

        let merge = tasks.iter().find(|t| t.id == "merge-X").unwrap();
        assert_eq!(
            merge.depends_on,
            vec!["A".to_string(), "B".to_string(), "C".to_string(), "D".to_string(), "E".to_string()]
        );
    }

    #[test]
    fn skip_existing_merge_nodes_idempotent() {
        // 如果重跑 inject，已存在的 Merge 节点不应被再次"包装"
        let mut tasks = vec![work("A", &[]), work("B", &[]), work("X", &["A", "B"])];
        inject_merge_nodes(&mut tasks, InjectOptions { enabled: true });
        let after_first = tasks.len();

        // 重跑：X 现在只 depends_on [M]，单 parent 不会再注入；
        // 已生成的 M 节点 kind=Merge，也会被跳过
        inject_merge_nodes(&mut tasks, InjectOptions { enabled: true });
        assert_eq!(tasks.len(), after_first, "second inject should be no-op");
    }

    #[test]
    fn multiple_downstream_tasks_independent_merges() {
        // X 依赖 (A,B)，Y 依赖 (B,C)；应该各自有独立 merge node
        let mut tasks = vec![
            work("A", &[]),
            work("B", &[]),
            work("C", &[]),
            work("X", &["A", "B"]),
            work("Y", &["B", "C"]),
        ];
        let added = inject_merge_nodes(&mut tasks, InjectOptions { enabled: true });
        assert_eq!(added, 2, "two independent diamonds → 2 merge nodes");

        let x = tasks.iter().find(|t| t.id == "X").unwrap();
        let y = tasks.iter().find(|t| t.id == "Y").unwrap();
        assert_eq!(x.depends_on, vec!["merge-X".to_string()]);
        assert_eq!(y.depends_on, vec!["merge-Y".to_string()]);
        assert_ne!(x.depends_on[0], y.depends_on[0]);
    }

    #[test]
    fn merge_node_carries_parents_in_merge_parents_field() {
        let mut tasks = vec![work("A", &[]), work("B", &[]), work("X", &["A", "B"])];
        inject_merge_nodes(&mut tasks, InjectOptions { enabled: true });
        let merge = tasks.iter().find(|t| t.kind == NodeKind::Merge).unwrap();
        assert_eq!(merge.merge_parents, vec!["A".to_string(), "B".to_string()]);
        assert_eq!(merge.depends_on, merge.merge_parents);
    }

    #[test]
    fn merge_node_title_mentions_parents() {
        let mut tasks = vec![work("A", &[]), work("B", &[]), work("X", &["A", "B"])];
        inject_merge_nodes(&mut tasks, InjectOptions { enabled: true });
        let merge = tasks.iter().find(|t| t.kind == NodeKind::Merge).unwrap();
        assert!(merge.title.contains("Task A"));
        assert!(merge.title.contains("Task B"));
        assert!(merge.title.starts_with("Merge:"));
    }

    #[test]
    fn merge_node_title_collapses_when_many_parents() {
        // N≥4 时 title 不再罗列所有 parent，避免 UI 标题过长
        let parent_ids: Vec<String> = (0..6).map(|i| format!("P{i}")).collect();
        let parent_refs: Vec<&str> = parent_ids.iter().map(|s| s.as_str()).collect();
        let mut tasks: Vec<PlannerTask> = parent_ids.iter().map(|p| work(p, &[])).collect();
        tasks.push(work("X", &parent_refs));

        inject_merge_nodes(&mut tasks, InjectOptions { enabled: true });
        let merge = tasks.iter().find(|t| t.kind == NodeKind::Merge).unwrap();
        assert!(merge.title.contains("(6 parents)"), "got title: {}", merge.title);
        assert!(merge.title.contains("Task P0"));
        assert!(merge.title.contains("Task P5"));
    }

    // ============================================================================
    // 集成式断言：与 validate_task_graph + consumes 校验交互
    // ============================================================================

    /// 注入后调用 `validate_task_graph` 仍然通过（无环 / dep ref 全部命中 / role
    /// 缺省合法）。这是 inject 的 critical safety invariant：算法不能因为加 merge
    /// 节点改写 depends_on 而引入悬挂引用或环。
    #[test]
    fn injection_preserves_validate_task_graph_invariants() {
        use crate::agent::planner::validate_task_graph;

        for n_parents in 2..=8 {
            let mut tasks: Vec<PlannerTask> = (0..n_parents)
                .map(|i| work(&format!("P{i}"), &[]))
                .collect();
            let parent_ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
            // X 依赖全部 parent
            tasks.push(work("X", &parent_ids));

            // inject 前 validate 应通过
            assert!(
                validate_task_graph(&tasks).is_ok(),
                "pre-inject N={n_parents} should validate"
            );

            inject_merge_nodes(&mut tasks, InjectOptions { enabled: true });

            // inject 后 validate 仍要通过——这是核心安全保证
            assert!(
                validate_task_graph(&tasks).is_ok(),
                "post-inject N={n_parents} must still validate (no cycles, no dangling refs)"
            );

            // X 的 depends_on 必须收敛到唯一 merge
            let x = tasks.iter().find(|t| t.id == "X").unwrap();
            assert_eq!(
                x.depends_on.len(),
                1,
                "post-inject X must depend on exactly 1 merge, got {:?}",
                x.depends_on
            );

            // merge 节点数量恒为 1（v1.1 修正：不再是 N-1）
            let merges = tasks.iter().filter(|t| t.kind == NodeKind::Merge).count();
            assert_eq!(merges, 1, "N={n_parents} → expected exactly 1 merge node");
        }
    }

    /// inject 不破坏 consumes_artifacts 语义：下游 X 原本 `consumes` 一个 parent
    /// 的 artifact，注入 merge 节点后 X.depends_on = [merge-X]，但 P_i 仍在 X
    /// 的**传递依赖闭包**内（X → merge-X → P_i）。
    ///
    /// 这里只做拓扑闭包的间接断言：从 X 出发 BFS 应该能走到所有原 parent。
    #[test]
    fn injection_preserves_transitive_closure_to_original_parents() {
        use std::collections::{HashSet, VecDeque};

        let mut tasks = vec![
            work("P1", &[]),
            work("P2", &[]),
            work("P3", &[]),
            work("P4", &[]),
            work("X", &["P1", "P2", "P3", "P4"]),
        ];
        inject_merge_nodes(&mut tasks, InjectOptions { enabled: true });

        // 反向邻接：task -> list of (dependency)
        let mut deps_by: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for t in &tasks {
            deps_by.insert(
                t.id.as_str(),
                t.depends_on.iter().map(String::as_str).collect(),
            );
        }

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back("X");
        while let Some(node) = queue.pop_front() {
            if !visited.insert(node) {
                continue;
            }
            if let Some(deps) = deps_by.get(node) {
                for d in deps {
                    queue.push_back(*d);
                }
            }
        }

        for orig_parent in ["P1", "P2", "P3", "P4"] {
            assert!(
                visited.contains(orig_parent),
                "X's transitive closure must include original parent {orig_parent} \
                 (otherwise consumes_artifacts validator would fail), \
                 closure = {visited:?}"
            );
        }
    }
}
