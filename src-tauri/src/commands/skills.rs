//! FM-15 FR-02.4: Skill 元数据列表 IPC。
//!
//! Tauri command 仅返回 frontmatter（无 body），用于 UI 列表 + Planner V2 prompt
//! 的 `description` 渲染。运行时 Engine 装载 skill body 走内部 API
//! `crate::skills::global().get(...).body`，不通过 IPC。

use serde::Serialize;

use crate::skills::registry::SkillMeta;

#[derive(Debug, Serialize)]
pub struct ListSkillsResponse {
    pub skills: Vec<SkillMeta>,
}

#[tauri::command]
pub fn list_skills() -> Result<ListSkillsResponse, String> {
    let reg = crate::skills::registry::global();
    Ok(ListSkillsResponse {
        skills: reg.metas(),
    })
}
