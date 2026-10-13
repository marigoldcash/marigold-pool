# Working in this repository

Read `PLAN.md` first. The work is its step list, M0 to M11, done strictly in order, one step at a time: build what the step says, write the tests it names, run its ✅ *Verify*, and tick the box only when the verify passes, in the same commit as the last change. Never tick a box on a build that passed without the tests, and never skip a verify because it needs the rig; if a verify cannot be run, the step stays open and the message says why.

The ground rules in `PLAN.md` are binding, in particular: builds and tests run on pve3 through `scripts/remote-build.sh`, never on this machine; commits are authored as `Dom <leancode@users.noreply.github.com>` with the step ID first in the message and no AI attribution anywhere; the smallest unit is the petal and the money bucket is a wallet; everything a person can read is written for someone's grandmother; nothing on the rig is started, enabled, opened in a firewall or killed by name.

Steps marked **[marigold]** are done in the node checkout at `../marigold`, committed and pushed there, and the dependency pin here is moved afterwards.

The node repository's `docs/marigold/REPOS.md` maps every Marigold repository and sets the one-checkout-per-session rule; it is the page to read when unsure which repository a change belongs in.

Two skills under `.claude/skills` apply to every Markdown edit and every commit message here; read them before the first of each.
