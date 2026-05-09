// Bound the number of tool-call rounds pi will perform per task.
//
// pi-coding-agent has no built-in max-turns / max-iterations knob; the
// only first-class limit on a delegated task is the wall-clock
// `PI_TIMEOUT` we apply in `scripts/pi-exec.sh`. That's blunt — a model
// stuck in a tool loop happily burns the full budget and reports a
// timeout failure with nothing useful to show for it.
//
// This extension intercepts the `tool_call` event and, after
// `PI_MAX_TURNS` calls (default 8), blocks every subsequent one with a
// reason string the model receives back. The model then has to wrap up
// with an answer based on what it already has, or hit the wall-clock
// timeout if it really refuses.
//
// pi runs as a fresh `--print` subprocess per delegated task (see
// pi-exec.sh), so the module-scope counter resets implicitly between
// tasks. The session_start handler also resets it for safety in case
// the same instance ever drives multiple sessions (interactive use,
// `/new`, `/resume`).
//
// Mount this file into the container at
// `/home/lmao/.pi/agent/extensions/max-turns.ts` — pi auto-discovers
// any `.ts` under that path.

import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";

const MAX_TURNS = Number(process.env.PI_MAX_TURNS ?? 8);

let count = 0;

export default function (pi: ExtensionAPI) {
    pi.on("session_start", async () => {
        count = 0;
    });

    pi.on("tool_call", async () => {
        count += 1;
        if (count > MAX_TURNS) {
            return {
                block: true,
                reason:
                    `max turns (${MAX_TURNS}) reached for this task. ` +
                    `Stop calling tools and answer the user with what you ` +
                    `have so far.`,
            };
        }
    });
}
