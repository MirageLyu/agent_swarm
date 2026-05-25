mod init;
pub mod llm_merge;
mod merge_strategy;
mod worktree;

pub use init::ensure_git_repo;
pub use llm_merge::{
    apply_resolved_merge, collect_conflict_blobs, merge_with_llm, ConflictBlob,
    LlmConflictResolver, LlmMergeOutcome,
};
pub use merge_strategy::{
    merge_branch_ref_only, ConflictResolution, LayeredMergeOutcome, MergeLayer, MergeStrategy,
};
pub use worktree::{
    DiffFile, MainCommitOutcome, MergeOutcome, TaskBaseConflict, TaskBaseOutcome,
    TaskBaseParentSummary, WorktreeManager,
};
