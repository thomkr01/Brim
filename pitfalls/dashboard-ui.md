# Pitfalls: Dashboard UI notes in the root file

Index and rules: [PITFALLS.md](../PITFALLS.md).

**The MCP tokens page said "No tokens yet." when the list did not load.**
The owner could decide there was nothing to revoke while live tokens existed. *Avoid:* a list keeps a `failed` signal from its load. The empty fallback shows "Could not load ..." with Retry when it is set.

See [crates/dashboard/ui/PITFALLS.md](../crates/dashboard/ui/PITFALLS.md) for the two worst ones (#143): a `>` in a `view!` attribute ends the tag, and reading a signal after its page is gone crashes the whole dashboard. Also:

- **List closures that call `.get`** rebuild every child on each change and kill CSS transitions (#99). *Avoid:* read per element with a `Memo` and `class:`.

- **The UI crate is not in the cargo workspace.** `cargo clippy` at the root does not check it. *Avoid:* run `cargo clippy --target wasm32-unknown-unknown -- -D warnings` in `crates/dashboard/ui`.

- **The debug wasm is about 100 MB** and makes the page close silently in Playwright. *Avoid:* use `trunk build --release` for screenshots and the e2e sweep.

- **A refused save vanished with the toast** (8d72d50). *Avoid:* an error the user must fix stays up until it is fixed or clicked away.

- **Auto Setup's steps read their state from the toast text** (#151). The toast closes itself after 3 s, so "Running" and "Done" vanished and the steps never stayed green. *Avoid:* keep a run's state in its own signal and show the result in the page. The toast only announces it.

- **Pages everyone may open still need greyed write buttons.** Lockdown was `Full` for viewers, and History's Roll back, Overview's Fix buttons and system switches looked usable although the server refused. *Avoid:* grey the write buttons for levels that cannot write.

- **A user choice kept only in the DOM is lost on reload.** The light/dark toggle only set `data-theme`, so a reload went back to dark. *Avoid:* save a choice the user makes in the UI (here in `localStorage`, read again in `main`).

- **A dropdown option's `selected=` stops working once the user has picked** (#158). The attribute only sets the default, so after Cancel the dropdown still showed the old pick. *Avoid:* use `prop:selected=` on every `<option>`, and `prop:value`/`prop:checked` on inputs (done in `form.rs`, `embeds.rs`, `lockdown.rs`; no plain `selected=`/`checked=`/`value=` bindings are left). The e2e sweep picks a channel on Logging, presses Cancel and checks the dropdown. It waits for the page to load first, because a pick made before the config arrives is overwritten by it.

- **Role names from Discord already start with `@` for @everyone** (#158). The dashboard added its own `@` and showed `@@everyone`. *Avoid:* trim a leading `@` before adding one (done in `form::items` and the `md.rs` role mentions; `recovery.rs` and the wizard's ticket pings already did). The bot never writes a role name after `@`.

- **The toast looked like two pieces.** A coloured `border-left` on a rounded box curves at the corners, and the absolutely placed progress strip starts inside that border, so the two met as a hook. *Avoid:* one colour mark per box. Put strips and edges inside an `overflow:hidden` box with `left:0; right:0` and no border on that side. Also `cargo fmt` in `crates/dashboard/ui` reformats unrelated files; revert them before committing.

**The verify page's button stayed dead after a lost connection, and the raid page was wider than a phone.**
The verify script caught a bad answer but not a failed `fetch`: with no connection the button stayed disabled with no text. A 32-character name with no space ran out of the raid report page at 375 px. Server-made pages were tested only on the good path at desktop width. *Avoid:* a script on such a page wraps `fetch` in `try`, shows a message and frees the button; the page body has `overflow-wrap:anywhere`. Only the verify page and the raid report page are server-made with a script or a list.

- **A reply was written into the message opened after it.** `save` wrote `cur` after its awaits, so switching to another message mid-save put the first one's id and Sent state into the second. *Avoid:* an async reply checks that the same item is still open (an open counter) before it writes page state.

- **Auto Setup lost its "running" line.** The simpler Auto Setup page dropped the `<Working />` line, so a one-minute run showed nothing until the result. *Avoid:* every action that can take more than a few seconds shows the shared `Working` indicator while it runs.

- **A loading message blinked instead of using `Working`.** The Update page's queued note had no moving indicator. *Avoid:* a loading message or status line uses the shared `Working` component; only an item that is itself loading (placeholder bar, running step) may pulse, through the one `.skel, .checks li.run` rule in `style.css`.

- **Loading dots blinked or froze with system animations off.** `.working` swapped the glide for an opacity pulse under reduced motion. *Avoid:* `Working` keeps its normal movement under reduced motion; only that component is exempt from the global reduced-motion rule.

- **Reduced-motion rules.** Animations are always on. There is no motion switch and no `data-motion`; a new rule must not stop animations for any setting. Only the hover lift may be removed.

- **The Wizard restarted at step 1 after a visit to another page.** The step and the Auto Setup picks lived only in signals of the open page. *Avoid:* state a person builds up over several steps is kept per server in `localStorage` (`wizard:<guild>`), cleared at the last step; never put a secret there. Unsaved edits inside a step's own cards are still lost.

- **The Update page's time left and bar only moved with the server answers.** Both were worked out from the server's whole-second step time once per poll, so a slow answer froze them and the native `<progress>` jumped. *Avoid:* a running timer counts on from the last answer with the page's own clock (250 ms tick) and the bar is a `.pbar` with a 1 s linear width transition.

- **Channel Setup showed the Channels grid under the Roles and Server tabs.** `.cs-grid` sets `display:grid`, which beats the browser's `[hidden]` rule, so `hidden=` did nothing (same for any element with its own display rule, such as the role picker). *Avoid:* `style.css` ends with `[hidden] { display:none !important; }`; a new class with a `display` value needs no extra rule for `hidden`. The browser test switches the tabs and checks that the grid is gone.

- **Spacing was off the 4 px grid in 233 places.** Tokens such as `space-6`, `space-10`, `space-18` and raw 2, 3, 6, 10 or 14 px values were used for margin, padding and gap. *Avoid:* every spacing is 4 px or a multiple of 4 (see `design/README.md`); `tools/ui-sizes.mjs` fails on a spacing off the grid, in `style.css` and `bundle.css`. A border or ring width uses `--stroke-2`, not a space token. A control whose inner offset changes (the toggle knob: track 44x24, knob 16 at 4 px) gets its size changed with it. Not covered by the lint: `em` spacing and positions set from Rust (`left`, `top` of popovers).

- **A wizard preset that is not on a card changes settings the owner never sees.** The first presets were two hidden switch names in `Card::preset`. *Avoid:* the wizard fills the values of `profile.json` for the section's paths that have no saved value, and a test fails for any value that is on no card (`wizard::tests::every_recommended_value_is_on_a_card`). A card that holds a number or a choice shows it with the schema's own field, so it saves with the same range check.

- **A wizard with a card of options for every protection was too hard to read.** The owner wants it simple: one switch and one sentence per protection (`security_step.rs`), the values from `profile.json` filled without being shown, all tuning on the module pages. *Avoid:* a new protection gets a row in `security_step::ROWS` and values in `profile.json`; a test fails for a value in a section with no switch (Misc fills its own). Nothing is added as a card field or an action picker in the wizard.
