### Added

- **Leaving `Plan` mode presents the plan.** Cycling the permission mode (`Shift-Tab`, or `/settings`' `permissions -> current mode` row — both paths already shared the identical "cycle the mode" action) out of `Plan` now opens an approval modal first, whenever the focused agent produced at least one reply while `Plan` was gating it, showing that reply as "the plan": `Enter` switches the mode (to `AutoAllow`) and sends a fixed, short turn (`Plan approved; proceed.`) so the model knows; `e` opens the plan text in `$EDITOR` first and sends the edited text as the turn instead, along with the same switch; `Esc` stays in `Plan`, switching nothing and sending nothing. The approval is also recorded as a `system_note` on the focused agent's own log, so `/context` and `conway sessions show` carry it. If the focused agent never produced a reply while `Plan` was active, or one of its turns is in flight at the exact instant the mode cycles, the switch stays silent, exactly as it always was — there is no plan to show yet in either case, and a turn settling afterward never pops the modal unprompted.

### Fixed

- `Plan` mode's own tool-gate guarantee ("no tool call runs until the mode has actually changed") is untouched by the new approval modal: the mode is written strictly before the approval/edited turn is ever sent, never after.
