//! Explicit Merge Node v1 —— planner 后处理：为多 parent 汇合点自动插入 merge 节点。
//!
//! ## 算法
//!
//! 对每个 `depends_on.len() >= 2` 的 task X，按"二叉 reduction tree"展开：
//!
//! ```text
//! N=2:  A,B → M(A,B) → X
//! N=3:  A,B → M1(A,B); M1,C → M2 → X
//! N=4:  A,B → M1; C,D → M2; M1,M2 → M3 → X
//! N=5:  A,B → M1; C,D → M2; M1,M2 → M3; M3,E → M4 → X
//! ```
//!
//! N parents 产生 **N-1** 个 merge node，深度 `ceil(log2(N))`。
//! X 最终只依赖**唯一** root merge node（X.depends_on = [root_merge_id]）。
//!
//! ## 为什么二叉
//!
//! 每个 merge agent 永远只看 **2 个** parent 的 diff，token 上下文小、可调试。
//! 用户原话："想要这个步骤有一个显式的节点来做 merge，有独立的 LLM 上下文和质量保证"。
//!
//! ## 为什么不动 consumes_artifacts
//!
//! 下游 X 原本 `consumes_artifacts = ["P1.foo"]`，P1 是其中一个 parent。
//! 注入 merge 节点后 X.depends_on 改成 [root_merge]，但 P1 仍在 X 的**传递依赖闭包**
//! 内（X → root_merge → ... → P1），所以 `planner_state::validate_consumes_artifacts`
//! 仍然通过——不需要重写 X 的 consumes_artifacts。
//!
//! ## 顺序敏感性
//!
//! parent 配对顺序按 `depends_on` 出现顺序保持稳定（不按完成时间，避免运行时
//! 拓扑因调度顺序漂移）。两个相邻 parent (i, i+1) 配成同一对，便于 UI 渲染
//! reduction tree 时的视觉一致性。
//!
//! ## 接入点
//!
//! `parse_and_validate` 中：validate → inject → revalidate。Inject 后必须重新
//! 校验 DAG（新增节点 + 改边可能形成环——理论上算法保证无环，但双保险）。

use crate::agent::planner::{NodeKind, PlannerTask};
use std::collections::VecDeque;

/// 注入选项；当前 v1 只暴露一个开关（避免硬切换：默认 false 保持旧行为）。
#[derive(Debug, Clone, Copy, Default)]
pub struct InjectOptions {
    /// 是否启用注入；false 时函数 no-op，所有 `Work` 节点不变。
    pub enabled: bool,
}

/// 对 `tasks` 原地注入 merge 节点。返回新增的 merge node 数量（含分层）。
///
/// 行为：
/// - 跳过 `kind == Merge` 的任务（避免对已注入的 plan 二次处理，幂等性）
/// - 跳过 `depends_on.len() < 2` 的任务（单 parent / 根节点不需要 merge）
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
    let mut counter: usize = 0;

    // 给定 parent 列表，构造 reduction tree，返回 root merge node id。
    for (task_idx, parents) in plan {
        let downstream_id = tasks[task_idx].id.clone();
        let downstream_title = tasks[task_idx].title.clone();
        let root_id = build_reduction_tree(
            &downstream_id,
            &downstream_title,
            &parents,
            &mut counter,
            &mut new_nodes,
            tasks,
        );
        tasks[task_idx].depends_on = vec![root_id];
    }

    let added = new_nodes.len();
    tasks.extend(new_nodes);
    added
}

/// 构造 reduction tree，返回 root merge node 的 id。
///
/// `tasks` 用于查找 parent 的 title 拼接合并描述。`new_nodes` 收集生成的 merge node。
fn build_reduction_tree(
    downstream_id: &str,
    downstream_title: &str,
    parents: &[String],
    counter: &mut usize,
    new_nodes: &mut Vec<PlannerTask>,
    existing_tasks: &[PlannerTask],
) -> String {
    debug_assert!(parents.len() >= 2, "caller should filter < 2 parents");

    // 用 title lookup 表（含已生成的 merge node title），后续 merge node 描述也能取到。
    let lookup_title = |id: &str, news: &[PlannerTask]| -> String {
        if let Some(t) = existing_tasks.iter().find(|t| t.id == id) {
            return t.title.clone();
        }
        if let Some(t) = news.iter().find(|t| t.id == id) {
            return t.title.clone();
        }
        id.to_string()
    };

    let mut layer: VecDeque<String> = parents.iter().cloned().collect();

    while layer.len() > 1 {
        let mut next_layer: VecDeque<String> = VecDeque::with_capacity((layer.len() + 1) / 2);

        // 每轮从队头取 2 个配对；奇数余下的 1 个直接进下一层（与下一层产物再配对）。
        while layer.len() >= 2 {
            let p1 = layer.pop_front().unwrap();
            let p2 = layer.pop_front().unwrap();
            *counter += 1;
            let merge_id = format!("merge-{downstream_id}-{counter}");
            let p1_title = lookup_title(&p1, new_nodes);
            let p2_title = lookup_title(&p2, new_nodes);
            let merge_task = PlannerTask {
                id: merge_id.clone(),
                title: format!("Merge: {p1_title} + {p2_title}"),
                description: format!(
                    "Reconcile worktrees from two upstream tasks ({p1} and {p2}) into a clean, \
                     verified merge so that downstream task `{downstream_id}` ({downstream_title}) \
                     can build on top of a coherent base."
                ),
                complexity: "low".to_string(),
                depends_on: vec![p1, p2],
                expected_output: Some(
                    "A merged worktree where conflicts (if any) are resolved with intent \
                     preserved from both parents, and `verify_command` (if configured) passes \
                     with exit code 0."
                        .to_string(),
                ),
                role: None,
                additional_skills: Vec::new(),
                produces_artifacts: Vec::new(),
                consumes_artifacts: Vec::new(),
                file_scope_hints: Default::default(),
                kind: NodeKind::Merge,
                merge_parents: vec![],
            };
            // merge_parents 与 depends_on 重复一份（语义说明 "is merging these two"）
            let mut merge_task = merge_task;
            merge_task.merge_parents = merge_task.depends_on.clone();
            new_nodes.push(merge_task);
            next_layer.push_back(merge_id);
        }

        // 余下奇数 1 个 → 进下一层；它会和"下一层第一个 merge node"配对
        if let Some(odd) = layer.pop_front() {
            next_layer.push_back(odd);
        }

        layer = next_layer;
    }

    layer.pop_front().expect("reduction tree must yield root")
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
        let added = inject_merge_nodes(
            &mut tasks,
            InjectOptions { enabled: false },
        );
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
        assert!(merge_id.starts_with("merge-X-"));
        assert_eq!(tasks[3].kind, NodeKind::Merge);
        assert_eq!(tasks[3].depends_on, vec!["A".to_string(), "B".to_string()]);
        assert_eq!(tasks[3].merge_parents, vec!["A".to_string(), "B".to_string()]);
        assert_eq!(tasks[2].depends_on, vec![merge_id.clone()]);
    }

    #[test]
    fn four_parents_balanced_tree() {
        // A,B → M1; C,D → M2; M1,M2 → M3; X.depends_on = [M3]
        let mut tasks = vec![
            work("A", &[]),
            work("B", &[]),
            work("C", &[]),
            work("D", &[]),
            work("X", &["A", "B", "C", "D"]),
        ];
        let added = inject_merge_nodes(&mut tasks, InjectOptions { enabled: true });
        assert_eq!(added, 3, "4 parents → 3 merge nodes");
        assert_eq!(tasks.len(), 8);

        // 找出 X 的 depends_on（应为唯一 root merge）
        let x = tasks.iter().find(|t| t.id == "X").unwrap();
        assert_eq!(x.depends_on.len(), 1);
        let root_id = &x.depends_on[0];

        let root = tasks.iter().find(|t| &t.id == root_id).unwrap();
        assert_eq!(root.kind, NodeKind::Merge);
        // root 的 2 个 parent 都应该是 merge node
        for p in &root.depends_on {
            let parent = tasks.iter().find(|t| &t.id == p).unwrap();
            assert_eq!(parent.kind, NodeKind::Merge);
        }

        // 验证叶子 merge 是 (A,B) 和 (C,D)（按 depends_on 出现顺序保持稳定）
        let merge_nodes: Vec<_> = tasks
            .iter()
            .filter(|t| t.kind == NodeKind::Merge && t.depends_on.iter().all(|d| ["A", "B", "C", "D"].contains(&d.as_str())))
            .collect();
        assert_eq!(merge_nodes.len(), 2);
        let leaf1 = &merge_nodes[0].depends_on;
        let leaf2 = &merge_nodes[1].depends_on;
        // 两叶子合并对必须是 {A,B} + {C,D} 二者之一
        let pair1: std::collections::HashSet<&String> = leaf1.iter().collect();
        let pair2: std::collections::HashSet<&String> = leaf2.iter().collect();
        let ab: std::collections::HashSet<String> = ["A", "B"].iter().map(|s| s.to_string()).collect();
        let cd: std::collections::HashSet<String> = ["C", "D"].iter().map(|s| s.to_string()).collect();
        let ab_ref: std::collections::HashSet<&String> = ab.iter().collect();
        let cd_ref: std::collections::HashSet<&String> = cd.iter().collect();
        assert!(
            (pair1 == ab_ref && pair2 == cd_ref) || (pair1 == cd_ref && pair2 == ab_ref),
            "leaves should be {{A,B}} + {{C,D}}, got {leaf1:?} + {leaf2:?}"
        );
    }

    #[test]
    fn five_parents_odd_carry() {
        // A,B → M1; C,D → M2; (M1,M2 配对) → M3; (M3,E 余数配对) → M4
        // 即：第一层产生 M1, M2，剩余 E；第二层 M1+M2 → M3，剩余 E；第三层 M3+E → M4
        let mut tasks = vec![
            work("A", &[]),
            work("B", &[]),
            work("C", &[]),
            work("D", &[]),
            work("E", &[]),
            work("X", &["A", "B", "C", "D", "E"]),
        ];
        let added = inject_merge_nodes(&mut tasks, InjectOptions { enabled: true });
        assert_eq!(added, 4, "5 parents → 4 merge nodes");

        let x = tasks.iter().find(|t| t.id == "X").unwrap();
        assert_eq!(x.depends_on.len(), 1);
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
    fn multiple_downstream_tasks_independent_trees() {
        // X 依赖 (A,B)，Y 依赖 (B,C)；应该各自有 reduction tree（不共用 merge 节点）
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
        assert_eq!(x.depends_on.len(), 1);
        assert_eq!(y.depends_on.len(), 1);
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
}
