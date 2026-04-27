//! FM-15 FR-02: Skill Registry。
//!
//! Skill = 一个可复用的"知识包"，由 `SKILL.md` 描述（YAML frontmatter + Markdown body）。
//!
//! Phase 1 (S3-2) 范围：
//! - 6 个 builtin skill（`include_str!` 进二进制）
//! - 用户级 / 项目级 SKILL.md 目录扫描（FR-02.1 / FR-02.3）
//! - `list_skills` IPC（FR-02.4）—— 仅返回 frontmatter（渐进披露）
//! - `get_body` 内部 API（FR-02.5）—— 运行时 Engine 注入用，不暴露给前端
//!
//! 暂不做：热加载、SKILL 编辑、远程 skill 下载（Phase 2+）。

pub mod registry;

pub use registry::{Skill, SkillFrontmatter, SkillRegistry, SkillSource};
