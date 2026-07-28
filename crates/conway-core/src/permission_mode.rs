//! Permission modes (V2): how much the operator is asked.
//!
//! [`PermissionMode::Prompt`] is the default and is exactly Conway's
//! pre-V2 behavior — every distinct call reaches the operator's gate.
//! The other two modes trade approvals for reach, in opposite directions:
//! [`PermissionMode::Plan`] answers *more* calls (by denying the mutating
//! ones outright), and [`PermissionMode::AutoAllow`] answers more by
//! allowing them.
//!
//! ## Plan mode is defined on `ToolCategory`, not on command text
//!
//! The tempting definition — "reads are fine, writes are not" — founders
//! on `bash cat file`, which reads a file using a tool that can do
//! anything. Deciding that by inspecting the command means parsing shell,
//! and a parser that is wrong once is a hole.
//!
//! So plan mode never looks at the command. It looks at the category the
//! *tool itself* declares. `bash` declares [`ToolCategory::Execute`]
//! regardless of what it is handed, so `bash cat file` is blocked in plan
//! mode — correctly, because the operator has no guarantee the next
//! invocation is equally benign. That is a property the gate can evaluate
//! rather than an intent it has to infer.
//!
//! ## `ToolCategory` is `#[non_exhaustive]`, so the match defaults to DENY
//!
//! [`PermissionMode::allows_category`] matches explicitly on the allowed
//! set and denies everything else, including variants that do not exist
//! yet. A future `ToolCategory::Deploy` is blocked in plan mode the day it
//! is added, without anyone remembering to update this file. The inverse
//! spelling — denying a list and allowing the rest — would silently permit
//! it, which is the wrong direction for a safety default.

use crate::content::ToolCategory;
use serde::{Deserialize, Serialize};

/// How much the operator is asked before a tool runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Ask for every distinct call. Conway's default and its pre-V2
    /// behavior.
    #[default]
    Prompt,
    /// Allow non-mutating categories without asking; deny everything else
    /// outright. For exploring a codebase without the agent changing it.
    Plan,
    /// Allow everything without asking. Deliberately the most visible mode
    /// in the status line — see `view/status.rs`.
    AutoAllow,
}

impl PermissionMode {
    /// The label shown in the status line's `mode` field.
    ///
    /// `AutoAllow`'s label is emphatic on purpose. An operator who has
    /// forgotten they are in it, and believes they are still being asked,
    /// is the failure this mode most needs to avoid.
    pub fn label(self) -> &'static str {
        match self {
            PermissionMode::Prompt => "prompt",
            PermissionMode::Plan => "plan",
            PermissionMode::AutoAllow => "AUTO-ALLOW",
        }
    }

    /// Whether plan mode permits `category`.
    ///
    /// Only meaningful for [`PermissionMode::Plan`]; the other modes do
    /// not consult categories at all. See this module's doc for why the
    /// match is spelled allow-list-then-deny rather than the inverse.
    pub fn allows_category(self, category: ToolCategory) -> bool {
        match self {
            PermissionMode::Plan => matches!(
                category,
                // Non-mutating: observe and reason, never change.
                ToolCategory::Read | ToolCategory::Search | ToolCategory::Think
            ),
            // Prompt and AutoAllow do not gate on category.
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_is_the_default() {
        assert_eq!(PermissionMode::default(), PermissionMode::Prompt);
    }

    #[test]
    fn plan_allows_only_the_non_mutating_categories() {
        let plan = PermissionMode::Plan;

        for allowed in [
            ToolCategory::Read,
            ToolCategory::Search,
            ToolCategory::Think,
        ] {
            assert!(
                plan.allows_category(allowed),
                "plan mode must permit {allowed:?}"
            );
        }

        for blocked in [
            ToolCategory::Edit,
            ToolCategory::Delete,
            ToolCategory::Move,
            ToolCategory::Execute,
            ToolCategory::Fetch,
            ToolCategory::Delegate,
        ] {
            assert!(
                !plan.allows_category(blocked),
                "plan mode must block {blocked:?}"
            );
        }
    }

    /// `bash` declares `Execute` no matter what command it is handed, so
    /// plan mode blocks `bash cat file` without anyone parsing shell.
    /// This is the whole reason the rule is written on categories.
    #[test]
    fn plan_blocks_execute_so_a_read_shaped_shell_command_is_still_blocked() {
        assert!(
            !PermissionMode::Plan.allows_category(ToolCategory::Execute),
            "a read-shaped bash command must still be blocked -- the tool's \
             declared category is what the gate can actually evaluate"
        );
    }

    /// The load-bearing property of the match's spelling: a category the
    /// enum does not have yet must be DENIED, not allowed.
    ///
    /// `ToolCategory` is `#[non_exhaustive]`, so this cannot be tested by
    /// constructing a novel variant from outside the crate. Instead it
    /// asserts the observable consequence: every category that is not one
    /// of the three named non-mutating ones is blocked, so the wildcard
    /// arm is a deny.
    #[test]
    fn plan_denies_by_default_so_a_future_category_is_blocked_not_allowed() {
        let all_known = [
            ToolCategory::Read,
            ToolCategory::Edit,
            ToolCategory::Delete,
            ToolCategory::Move,
            ToolCategory::Search,
            ToolCategory::Execute,
            ToolCategory::Think,
            ToolCategory::Fetch,
            ToolCategory::Delegate,
        ];
        let allowed: Vec<_> = all_known
            .iter()
            .filter(|c| PermissionMode::Plan.allows_category(**c))
            .collect();
        assert_eq!(
            allowed.len(),
            3,
            "exactly the three non-mutating categories may be allowed; \
             anything else (including a future variant) must fall through \
             the wildcard arm to deny"
        );
    }

    #[test]
    fn other_modes_do_not_gate_on_category() {
        for mode in [PermissionMode::Prompt, PermissionMode::AutoAllow] {
            assert!(mode.allows_category(ToolCategory::Execute));
            assert!(mode.allows_category(ToolCategory::Delete));
        }
    }

    /// The auto-allow label is deliberately shouty: an operator who has
    /// forgotten they are in it is this mode's core risk.
    #[test]
    fn auto_allow_labels_itself_unmistakably() {
        assert_eq!(PermissionMode::AutoAllow.label(), "AUTO-ALLOW");
        assert_eq!(PermissionMode::Prompt.label(), "prompt");
        assert_eq!(PermissionMode::Plan.label(), "plan");
    }
}
