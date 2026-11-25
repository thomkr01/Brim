# Dashboard UI pitfalls

Pitfalls for the rest of the bot are in [PITFALLS.md](../../../PITFALLS.md) at the repo root.

These are Leptos mistakes that broke the dashboard in November 2026. They compile without a warning, so read this before you change the UI. The e2e sweep (`tests/e2e/dashboard.js`) checks for each of them.

## 1. A `>` in an attribute ends the tag

`view!` reads an attribute value without braces only up to the next `>`. So `<button disabled=move || lo >= num on:click=move |_| step(-1.0)>"-"</button>` becomes `<button disabled=move || lo>` plus the text `= num on:click=move |_| step(-1.0)>-`. It compiles, but the page shows code as text and the button has no handler. A `<` is safe (`hi <= num` worked).
*Avoid:* wrap a value that holds `>` or `>=` in braces: `disabled=move || { lo >= num }`, or flip it: `num <= lo`. Expressions inside `(...)` are safe (`.is_some_and(|a| a.len >= 25)`). Check: the sweep fails if a page shows `move |` or `on:click=` as text (case-insensitive, CSS can capitalize).

## 2. Reading a signal after its page is gone

A page's signals and `StoredValue`s are disposed when you leave the page. Most reads of a disposed value panic ("Tried to access a reactive value that has already been disposed", `reactive_graph-0.2.16/src/traits.rs`). A wasm panic cannot be recovered, so the whole dashboard stops ("Dashboard failed to start: RuntimeError: unreachable executed"); reload to get it back. It happens when code runs after the page is gone:
- the rest of a `spawn_local` task after an `.await`;
- a `set_timeout`, `request_animation_frame` or `later(...)` callback;
- a `load` closure called again after an await (`Ok(_) => load`), because `load` reads `g.get_value` or `page.get_untracked`.

A slow or failing server (for example the restart during Update) makes it likely.

| Call on a disposed value | Result |
|---|---|
| `.get`, `.get_untracked`, `.with(..)`, `.with_untracked(..)`, `.read` | **panics** |
| `StoredValue::get_value`, `with_value(..)` | **panics** |
| `NodeRef::get_untracked` | **panics** |
| `.set(..)`, `.update(..)`, `update_value(..)` | does nothing, safe |
| `.try_get_untracked`, `.try_with(..)`, `.is_disposed` | returns `None` / `true`, safe |

*Avoid:*
1. After every `.await` in a `spawn_local`, check the page still exists before reading:
   ```rust
   let r = api("GET", &path, None).await;
   if data.is_disposed {
       return;
   }
   ```
2. A closure that can run again after an await (`load`, `load_mine`, ...) starts with `if g.is_disposed { return; }`.
3. In timers and frame callbacks read with `try_` (`msg.stay.try_get_untracked == Some(false)`, `pv.try_get_untracked.flatten`).
4. Use `run_act`, `run_change` and `run_save` (`common.rs`) for actions: they skip the callback if the page is gone. Do not write your own `spawn_local`.
5. Setting a value needs no check. Only reads panic.

Do not switch tasks to `spawn_local_scoped_with_cancellation`: it also cancels when only part of the page goes (a dialog that closes itself), so the save's follow-up never runs. Check: the sweep switches through every page with each API response held 800 ms, then with every request failing. Any panic fails.

## 3. A hover rule beats the "selected" rule

`nav a:not(.srv):hover` has higher specificity than `nav a.on` (`:not(.srv)` counts as a class). The selected link got the dark hover background with dark text (1.1:1).
*Avoid:* a selected/active rule lists its `:hover` variant with the same selectors: `nav a.on, nav a.on:not(.srv):hover { ... }`. Check: the `contrast` phase of `tests/e2e/deep.js` hovers every link and button on every page in light and dark and fails under 3:1.

## 4. A page that reads a context signal is rebuilt when the signal is set again

The page closure in `App` read `access.of(p)`. Access is fetched on every page change, and setting a `RwSignal` notifies even for the same value, so every page was built twice (settings, schema, Discord data loaded twice). It was also built before the access answer, so hidden pages still sent requests.
*Avoid:* read a value that is fetched again through a `Memo` (`page_access`), which notifies only on change. Decide what to show before building the page's components.

## 5. A dropped browser promise hides a refusal

`let _ = clipboard.write_text(..)` dropped the promise: the button said "Copied!" when the browser refused, and the refusal was an uncaught page error (also for the Rescue Key, shown once).
*Avoid:* await browser promises with `JsFuture` and show the result. Use `copy_text` and `copy_label` in `common.rs` for copy buttons.

## 6. An emptied number field saved `null`

Clearing a number field stored `null`, but most number settings are plain `i64`/`u32`/`f64`, so the save failed ("invalid type: null, expected i64"). A float step also kept float error (`0.01 + 1 - 1` = `0.010000000000000009`).
*Avoid:* `Field` sets the default when a number input is emptied, and float steps are rounded like percent steps. A new number field gets a default (`dv`) unless `null` means something.

## 7. A descendant selector styles more than the field title

`.row label { flex:0 0 100%; font-weight:700 }` meant for a field's title also hit the Import button of every list field (a `<label>` around the file input): full row, bold, help text drawn over it. It also made checkboxes in role and channel lists bold and capitalized. The first fix (`.io label`) did nothing: same specificity as `.row label`, which comes later in the file.
*Avoid:* a label that wraps a control (`.row label:has(> input)`) resets what `.row label` sets. To beat a rule, use higher specificity, not file order, and check in the browser (`DEEP_ONLY=overflow`). A list field's box takes the full row so help text goes below.

## 8. Layout breaks that only show at some widths or with long text

Switch help text was squeezed into thin columns, and at 390 px the header was wider than the screen (avatar cut off, search over the logo).
*Avoid:* a switch's help text sits on its own line under the label (`.tog > .muted`). On a phone, header links that do not fit go into the account menu (`.m-only`). `DEEP_ONLY=overflow node tests/e2e/deep.js` checks every page at 1400, 1024 and 390 px for text outside its box, cut off, overlapping or squeezed.

## 9. One version per widget on a page that saves several sections

Each Overview switch loaded the config and kept its own version, so after one saved, the next sent an old version and was refused ("config changed, reload"). Five switches also meant five identical config requests.
*Avoid:* widgets on one page that save into the same config share one loaded config and one version signal, and update it after each save.

## 10. Many copies of one SVG icon share its ids

Icons in `assets/icons` have fixed ids (`info-grad`, `info-cut`). With many copies on a page, all paint with the first copy's gradient and mask, and draw nothing once it is hidden (Chrome does not render a gradient from a `display:none` SVG). A first fix (`icon_unique` for the info icon only) left arrows, check rows and sidebar icons sharing ids: uniqueness was opt-in.
*Avoid:* `icons::icon` numbers the ids of every copy; it is the only way `icon` works. Never inline an `assets/icons` SVG another way (`include_str!` into `inner_html`) without the same numbering.

## 11. A transform on a parent moves its popup too

`transform:scale` on `.tip` (info icon hover) also scaled the card inside, so it was wider than measured and crossed the screen edge on a phone.
*Avoid:* animate the icon (`.tip .i`), not the element that holds the popup. `Tip` (`common.rs`) places the card on open: shifted to stay inside `main` and the screen, below the icon near the header. `DEEP_ONLY=overflow` opens every info card and checks it.

## 12. `AnimatedShow` can hide itself while `when` is true

`AnimatedShow` starts a hide timer each time it reruns with `when` false but remembers only the last. A form that loads sets `val` and `orig` one after the other, so `when` (`val != orig`) reruns false twice; the first timer fires later. A change within 200 ms shows "Changes detected!" and hides it at once, so nothing can be saved or cancelled until the next change (#158).
*Avoid:* use `common::Fade` (it puts `when` behind a `Memo`), not `AnimatedShow`. Same class: the page wrapper read the access level straight from the signal, so every access answer rebuilt the open page and could drop edits; it reads a `Memo` of the level now. When a reactive closure builds a whole page or form, read derived values through a `Memo`, not a signal that is set again with the same content.

## 13. A hidden popup still makes the page wider

The info card (`.tip .bubble`) is only `opacity:0` until hovered, so it still takes layout room. Near the right edge at 390 px it pushed the page sideways.
*Avoid:* `main` has `overflow-x:clip` (clip, not hidden, so vertical overflow and sticky parts work). A new popup hidden with opacity or transform must also stay inside `main`, as `Tip`'s `place` does when shown. Check: at 390 px `document.documentElement.scrollWidth` equals the viewport width on every page.

## 14. An effect on a new input runs before its `prop:value` is set

An `Effect` on the input's `NodeRef` (Channel Setup's rename box) ran before `prop:value` was applied, so `select` selected an empty box and typing added to the old name. A `select` in `on:focus` had the same problem.
*Avoid:* an effect that needs a new input's value sets it first (`i.set_value(..)`, then `focus` and `select`).

## 15. Overlapping saves stored an older draft

Channel Setup sent a PUT per edit. Two in flight could arrive in either order, so the older draft could be stored. The refused-save path set the page from a follow-up GET without checking it was the newest request; an edit before the first GET answered built on an empty draft and replaced the stored one.
*Avoid:* a page that saves on every edit keeps one request in flight: later edits change only the local copy and mark it dirty, and the reply sends the newest copy if it differs. Every reply that sets page data after an await checks its sequence number with `try_get_value` (rule 2). Actions that need the saved state (Push, Discard) are disabled while a request is pending, and editing waits until the first load is in.

## 16. A panel rebuilt from every reply loses the focus

Channel Setup's right panel is rebuilt from each reply, so the control just used was replaced and keyboard focus fell to the page.
*Avoid:* a reactive closure that rebuilds controls notes the focused control's `aria-label` before building and focuses the new control with that label in a `request_animation_frame` (`panel_focus`/`refocus` in `channels.rs`). Every control in such a panel needs a unique `aria-label`.

## 17. A select that acts on `change` fires on every arrow key

In Chrome on Windows and Linux an arrow key on a closed `<select>` changes its value and fires `change`. The "+ Role" picker added every role arrowed past.
*Avoid:* a select that creates or adds something only picks; a button does the action (`kind to add` + "+ Add", `role to add` + "+ Role" in `channels.rs`). A select that only sets a value saved later (settings forms) can keep `on:change`.

## 18. `document.activeElement` does not say which button was clicked

The shared 2FA prompt used `document.activeElement` to pick the control to click again. Safari (macOS and iOS) does not focus a button on click, so the retry could press a different button with its confirm dialog skipped.
*Avoid:* use `common::clicked` (a capture-phase click listener records the control and clears it after the click). Read it synchronously in the handler; after an `.await` it is gone, so capture it first and pass it on (`need_step_up(need, trigger)`).

## 19. A number field's +/- did not fit its range

Bugs in `Field` (`form.rs`). A `float` field with range 0.001-0.99 ("Heat degradation") stepped by 1, so + went to 0.99 and - to 0.001, never back to 0.01. A field without default and empty = off (Join Raid "Minimum matches", `id_flag_min`, range 2-500; Join Gate optional age override, duration from 5 s): + from empty gave the minimum, and - was disabled there, so it could not be turned off.
*Avoid:* a fraction with a range below 1 is a `pct` field (steps by 1 %), not a `float` (`similarity_floor` is `pct` now; `heat.max`, heat weights and `webhook_multiplier` step by 1 fine). A number field without a default keeps - enabled at the bottom of its range, and - there empties it (`at_floor` in `Field`, number and duration branch). A range that depends on another setting (heat weights up to `heat.max`) comes as `max_path` in the schema, and `Field` reads it from the section, so form and server agree.

## 20. A long string without spaces in a table cell widened the page

The Raider List evidence table shows Detail JSON (`{"case_kind":"ban","case_no":1}`) in a `<code>` cell with no break points; at 390 px it pushed past the card.
*Avoid:* `td code` wraps anywhere (`style.css`). A new cell with an id, JSON or URL uses `<code>` or its own `overflow-wrap:anywhere`.

## 21. The rescue key QR code was never blurred

The page gave the QR `class:blur`, but the CSS was `pre.blur`, so only the key text blurred. The key and QR shared one Show button, and a new key kept the earlier Show state.
*Avoid:* a secret QR gets its own `.qr.blur` rule (strong enough that it cannot be scanned) and its own Show button. When a class is added to a new element, check the CSS selector is not bound to one tag. Reset every reveal when a new secret is made.

## 22. "Too many requests, slow down" hid a 2FA lockout

Every 429 said "too many requests, slow down": the per-session request limit, the 2FA lockout after 5 wrong codes, and the bot profile's 30 changes an hour. After a 2FA reset the owner could not know he was locked out, or for how long.
*Avoid (owner rule):* state the issue, not that "there is a problem". An error says which limit or check was hit and what the user can do (wait how long, what to change). `ApiError::TooMany` takes its message, so every new limit writes its own. Say no more than the user needs: the lockout and its length, never what an attacker could use (which code was close, the counter).

## 23. A page loaded right after another showed nothing (request limit)

The per-session limit allowed 20 requests at once, and the Tickets tab alone sends about 12 on load, so a quick switch to the next page got 429 on `/config` and `/schema` and showed an empty page.
*Avoid:* `auth::BURST` (40) must hold two page loads in a row. A page that adds many requests on load needs a check against it; the steady rate (20 a second) stays the guard.

## Where #143 was fixed (places to check for the same pitfall as rule 2)

- `form.rs`: the `-` button on number fields (`>=` in the attribute); the settings card's config/schema load; the preview flash frame callback; list import.
- `update.rs`: the update status request.
- `pages.rs`: the setup score request.
- `common.rs`: the toast timer, and the shared `run` behind `run_act`/`run_change`/`run_save`.
- `appeals.rs`, `embeds.rs`, `lockdown.rs`, `records.rs`: the `load` closures that run again after an action.
- `main.rs`: config import after the file is read.

**An emptied text field saved null, and the save was refused.** (Tickets 2.0)
Greeting, closing, pause reason and ban appeal message are plain strings in the config, but the form sent `null` when cleared ("invalid type: null").
*Avoid:* an emptied number or text field saves its schema default. Give every text field that maps to a plain `String` a `dv(.., "")` default in schema.rs.

## 24. The account menu stayed open after a choice

The account menu and the bell are `<details>` in the header. A link in them changes only the hash, so the menu stayed open over the new page and moved with its button when the header changed. A tap outside did not close it. The Feeds ⋯ menu stayed open too, because the rule covered only the header.
*Avoid:* `close_menus` (main.rs) closes every open header `<details>` and every row ⋯ menu (`details.rowmenu`) on a click outside its own button, on a hash change and on Escape. A new dropdown is a `<details>` inside `header` or with class `rowmenu` and gets this free; a dropdown made another way must close on the same three events (the message builder's popups have a `.popbg` backdrop).

## 25. A system toggle jumped left while its toast showed

`Fade` wraps its child in `AnimatedShow`'s `<div>`. The toast is `position:fixed`, but the empty wrapper is not, so in the Overview card footer (`.sys .foot`, flex `space-between`) it became a third flex item and pushed the toggle to the middle.
*Avoid:* `Fade` gives its wrapper class `fade-wrap` (`display:contents`). A component that adds a wrapper element must not change the layout of where it is dropped; check it inside a flex row.

## 26. The Overview score did not follow a system toggle

A system toggle saved the config but did not reload the checks, so "Join gate is off" stayed until a page reload.
*Avoid:* `SystemToggle` takes a `saved` callback; the Overview passes one that reads the score again, not the config (that read could land after a second switch's save and show it in its old state). Another page that changes a setting needs nothing: the Overview loads both each time it opens.

## 27. A text box typed in place lost the cursor on every key

The Message Builder types on the message itself (`contenteditable`). A view that reads the whole message rebuilds every box on each key, and setting a box's HTML while it has focus puts the cursor at the start.
*Avoid:* rebuild the editor only from a `Memo` of the message's shape (embeds, fields, inline flags, buttons per row), never from the text. Each box (`Ed` in `embeds.rs`) writes its HTML only while it lacks focus and only when the text differs. The box handles Paste and Enter, so no other page's HTML gets in.

## 28. A popup that grows after it opened ran off the screen

The footer-time popup grows when "Date and time" adds the calendar, but the move-up check ran once on open, so the time box was cut off.
*Avoid:* a popup that moves itself to stay on screen checks again whenever its content can change (the Message Builder popup re-checks on every message change).

## 29. Typed JSON put text where the editor expected an object

The JSON view lets a user write `{"embeds": [1]}` or `"fields": ["x"]`. The editor then indexed by key (`e["fields"]`, `em["fields"][i]["inline"]`), and serde_json panics when a string or number is indexed mutably by key, or an array past its end.
*Avoid:* every write into the message goes through `pointer_mut`/`get_mut`/`as_object_mut` and skips a value of the wrong shape (`put`, `edit_embed`). A popup for an embed or button row closes when that embed or row is removed. The server refuses fields that are not objects.

## 30. A chip class name took a popup's layout rule

A member-variable chip with class `mo` matched `.pop .mo` (calendar month row, `display:flex`) and filled the whole line in a menu answer.
*Avoid:* short class names in popups are taken (`.pop .mo`, `.pop .row`, `.pop .seg`). Give new inline parts their own name (`vchip`, `mem`) and check them inside a popup too.

## 31. Escape did nothing while the text box kept the focus

Toolbar buttons keep the cursor in the text box (`mousedown` + `prevent_default`), so a popup opened from them never had focus and its `keydown` never saw Escape.
*Avoid:* Escape for the builder's popups is one `window_event_listener` (removed `on_cleanup`) that closes the top popup first.

## 32. `move || a || b` as a `view!` attribute did not compile as one closure

`disabled=move || busy.get || n >= max.get`: `view!` read the second `||` outside the closure (types mismatch); with parentheses it compiled but clippy refused the extra parentheses.
*Avoid:* an attribute value with `||` or `&&` in a closure is a named closure first (`let off = move || busy.get || n >= max.get;` then `disabled=off`).

## Enter that confirmed an IME word also submitted the box

For Chinese, Japanese or Korean, Enter picks the candidate word and fires `keydown` with `key == "Enter"` and `is_composing` true (keyCode 229 in older browsers). Every `if e.key == "Enter"` handler submitted or broke the line mid-word.
*Avoid:* a handler that acts on Enter calls `common::is_enter(&e)`, never a plain `e.key == "Enter"` check.

## A typed marker put link HTML into an attribute

`md::inline` stashes code spans, links and `<...>` tokens as `U+0001 n U+0001` and swaps them back in order on the whole string. `esc` leaves U+0001 alone, so an emoji named with a typed marker (`<:\u{1}1\u{1}:42>`) got the next link's HTML in its `alt`; the link's `"` closed the attribute and `onerror=` ran when an Owner opened the preview (stored XSS from Editor level).
*Avoid:* text that a stash-and-swap pass works on must not be able to hold the marker: `inline` drops U+0001 before stashing, and the server refuses control characters in saved messages. No other UI pass swaps placeholders back into HTML (`wizard::fill` fills JSON-escaped values into sample JSON, `icons::fill_tags` puts fixed svg only).

## A failed save hid the Save bar

The settings form and Permits set `orig` to the edited values before the request. On refusal (conflict, bad value, 2FA needed) the "Changes detected!" bar vanished, the fields kept the edit, and a later save of another section never sent it. After the 2FA code there was no button to press again.
*Avoid:* set the saved state (`orig`) only from a good server answer, per section when several are saved one after another (embeds.rs does this). A `saving` signal disables Save while a request runs, so a double click does not send the same version twice.

## Older answers overwrote newer ones

Record lists (`records::fetch`), appeals, the members search, the trigger "Try a message" box (a POST per key), the schedule "Next:" preview, and the sticky, trigger and schedule lists of a message wrote whatever answer came last, showing old rows under a new filter or message.
*Avoid:* every request whose answer sets page data checks after the await that it is still the newest: a sequence number (`StoredValue`, or `fetch`'s per-list counter) or the id it was asked for (`embed`). A request per key waits 300 ms after the last key (as `CaptchaPreview`).

## Typed text was cleared before the server took it

"Add note" and Lockdown "Send an update" cleared the box in the click handler, so a refusal lost the text. The appeal reply box was never cleared, so a reply for one member went out with the next decision on another.
*Avoid:* clear a text box in the success branch of its request, and clear a box that belongs to one item after that item's action worked.

## Message Builder: Cancel and Save did less than they showed

Cancel restored name, body and channel but not Pings and "Send as", although `snapshot` and the dirty check hold all five, so a picked "@everyone" stayed and was saved later. In the JSON view a text that did not parse kept the last valid body: Save said "Saved." and leaving gave no prompt. A plain Save did not set `busy`, so a double click on a new message inserted it twice.
*Avoid:* Cancel restores every part `snapshot` holds. Save and leaving check the JSON view's error first. Every save sets `busy`, and the bar's Save is disabled while it runs.

## A page built on a config it did not have

After an action, the settings form set the new version before reading the new config, so a failed read left old values with the new version and the next save passed the conflict check and overwrote the action. A failed first load showed defaults as if stored. Overview turned a failed `/config` into `{"config": {}}` and showed every system off.
*Avoid:* set a version together with the config it belongs to. A page whose config did not load says so and offers no Save or switches (locked) until a reload.

## Errors that hid the cause

`reply` showed "request failed" for every answer without our JSON error (413 for a large import, proxy 502 during an update) and the raw `JsError` for a network error. A failed emoji list showed "This server has no emojis of its own." and was cached; a failed `/schema` left search with page names only, silently; the raw JSON editor dropped the parser's line and column; a number box ignored non-number text.
*Avoid:* an error names the actual issue: status and text when the body is not ours, "Could not reach the dashboard server" for a send error, the parser's message. Show a failed list as failed and ask again next time, never as empty.

## A Rename approved an unreviewed AutoMod rule

The pencil sent the whole rule through `PUT`, and the server approves the rule on every save, so one click approved a change made in Discord without review. Bulk Activate/Deactivate fired all requests at once, each with its own reload, and a refused switch (already changed in Discord) reloaded nothing.
*Avoid:* a paused rule is renamed only through "Review and approve". Bulk actions run one after another, then reload once whatever the answers were, and show every error.

## A comma list split an entry that held a comma

`list` fields show `csv` and save `from_csv`, but Import adds one entry per line, so `hello, world` became two words on the next edit (a phrase filter became two single-word filters).
*Avoid:* a list that holds an entry with a comma shows one entry per line (the `lines` box). A comma typed into the one-line box still splits the entry.

## A one-click setup dropped unsaved edits

Logging, Catch and Tickets setups rebuild the form from the server after running, so edits typed before the click vanished, and the fresh version meant the conflict check could not protect them.
*Avoid:* `Section` reports its dirty state (`dirty` prop); an action that rebuilds the form is disabled with "Save your changes first" while it is set.

## The message preview loaded any image address

The preview used thumbnail, author, footer and image URLs as they were, without the editor's `https` check, so a saved embed could make a viewer's browser call any host.
*Avoid:* a preview loads only `https://` images; other addresses are left out.

## The dashboard font came from Google

`index.html` loaded Poppins from fonts.googleapis.com, sending every visitor's IP to Google, while PRIVACY.md says nothing goes to third parties.
*Avoid:* the UI loads nothing from another host: fonts, scripts and styles are files under `assets/` that Trunk copies into `dist` (`assets/fonts`, with its licence). The only exception is Cloudflare Turnstile on the verification page, which PRIVACY.md names.

## A bare `.chip` rule restyled every chip

The Message Builder block in `style.css` declared `.chip { background:none; ... }` for weekday chips after the base `.chip` rule, so every plain chip (OWNER, APP, lockdown chips) lost its fill. `@keyframes pulse` was also defined twice; the later won everywhere.
*Avoid:* scope a page's rule to its container (`.chipset .chip`). Define an animation name once (search `@keyframes` before adding one).

## A warning closed before it could be read

Every toast, errors included, closed after a fixed 3 s (`TOAST_LIFE`). `Msg` had a `stay` flag, but nothing set it, so the rule "a refused save stays up" (root PITFALLS, dashboard) was silently broken on every page.
*Avoid:* `msg.err` (errors and warnings) stays until clicked or replaced; an ok toast stays 3 s plus 50 ms per character past 60, and its strip runs as long. A warning about one item is also shown next to that item (the Sent log row keeps a copy's picture warning). The toast sits at the top so a stuck one never covers the Save bar on a phone.

## The account button moved between pages

Long pages get a scrollbar, short ones (server list, Commands) do not. With classic scrollbars (Mac with a mouse, Windows) it takes about 15 px, so the header's right side (bell, theme, account) jumped left on long pages. Headless test browsers hide scrollbars, so tests never saw it.
*Avoid:* `html { scrollbar-gutter: stable; }` on every page. To check a layout jump, launch the test browser without `--hide-scrollbars`.

## "Update now" showed when Brim was up to date

The Update page offered the update whenever Running and Newest were different commits. Running can be newer than Newest (Newest is cached for a minute, and images of a newer commit can be out before the list shows it), so each press restarted the bot and dashboard for nothing: "not the same" was read as "newer".
*Avoid:* ask GitHub's compare which is ahead (`update::newer`); offer the update only when Newest is ahead, and fall back to "not the same" only when GitHub cannot answer. The header's Update link has no "new version" mark, so nothing else compares the two.

## A double click posted twice

Send and Edit message started a request on every click, so a quick double click posted two copies. A row without an id would also have turned "Edit message" into a Send, since 0 meant "new copy".
*Avoid:* a button that posts to Discord waits while its request runs (a `busy` signal), and a missing id disables the action instead of falling back to a value with another meaning.

## An icon tag broke a link in the preview

`md` swaps `{e:name}` icon tags for icon HTML after the rest of the HTML is built, so a tag in a masked link's target (`[x](https://a/{e:case})`) put the icon's quotes into `href` and closed it early. The leftover attributes were fixed markup, so no script ran, but the link broke.
*Avoid:* a pass that runs on built HTML must not reach into attributes; first encode what it looks for (`{` and `}` as `%7B` / `%7D`) in every attribute value. `xss_tests.rs` fuzzes the preview renderers for this.
## A server error toast said only "internal error"

The toast gave no way to find the cause in the log. The server now sends "Server error, code XXXXXXXX. The cause is in the dashboard log." (or a recognised cause's plain text plus " (code XXXXXXXX)") and logs the cause under `error_id=XXXXXXXX` (root PITFALLS, section 6). `run_act` closes a refused action's toast by itself after about 3 s, which hid the code.

*Avoid:* show the server's error text as it is; never replace it with a fixed text. `run` keeps any toast that starts with "Server error, code " or ends with " (code XXXXXXXX)" until clicked. If the server text changes, change that check too.

## A new class name took another widget's style (score ring, table card, front page hero)

A class name that `style.css` already uses takes that rule. Only a render shows it; clippy, tests and the e2e sweep pass. Cases: the score ring's `pop` is the Message Builder's popup box (the number showed in a dark square); the table card's `.act` is the settings card's action row (flex column, 8 px top margin), so the Permits Edit cells became flex boxes; the front page hero's `.ring` is the dashboard score ring (the glow circle became an oval). Front page and dashboard share the stylesheet, and its rules are global.

*Avoid:* before adding a class, grep `style.css` for `\.<name>\b` (a bare rule of that name too). Pick a unique name, or scope the rule under its component (`.ring .num.landed`), and render the change once. The table button column is `.tact`. Front page classes start with `hero-` or `land-` (`.land .hero-...`). Preview a front page change with the real page (`trunk build`, serve `dist/`, answer `/api/*` with 401), not a static copy.

## Loading looked different on every page

Pages showed "Loading...", a button box with dots, or nothing while a slow action ran. Owner rule: the dots and fun word indicator is the one loading indicator.

*Avoid:* every loading or working state uses `<Working />` from `common.rs`: four dots and a fun word, no box (a button that shows it drops its box), the word changes every 4 s, the dots glide slowly. No spinner, "Loading..." text or other indicator. Under reduced motion the dots pulse in place, so the global reduced-motion rule must not stop them.

- **The published dashboard wasm was 37.7 MB because trunk skipped wasm-opt.** `index.html` had no `<link data-trunk rel="rust" ...>`, so trunk builds a default app with wasm-opt off (`new_default` in trunk's `pipelines/rust`). *Avoid:* the `rust` link stays in `index.html` with `data-wasm-opt`, and `[profile.release]` in `ui/Cargo.toml` stays on `opt-level = "z"`, `lto = "fat"`, `codegen-units = 1`. The built wasm is 3.2 MB (1.2 MB gzip, 0.87 MB brotli); check `dist/*_bg.wasm` after a build: a build near 30 MB again lost the link.

## A page drew its "not available" state before the first answer (Update, Commands)

The Update page started with `data = Null` and drew at once "Running: unknown", "Could not reach GitHub right now." and "The updater is not running" until `/api/update` answered. Commands did the same ("The bot has not published its commands yet"). Every field of a `Null` value reads as missing, and a missing field was taken as "no".

*Avoid:* a page that loads one `Value` shows `<Working />` while it is `Null` and draws no empty, "unknown" or "not running" state until the first answer is in. A first answer that fails shows an error line, not the loading sign for ever.

## "Diff vs current" did nothing you could see (History)

The button wrote its answer into a `<pre>` at the bottom of the page, below 200 version rows, the layout versions and the AI reads. The answer was off screen, with no loading sign, and a failed history load was dropped without a message.

*Avoid:* the answer to a button goes where the person is looking: a dialog (as the settings import preview) with `<Working />` while it loads and an error line that states the cause. Never write a result into a block far from the button. Every load that can fail shows its error (`Err` is not dropped). History rows show the member's name (`records::learn_names` + `records::who`) with the ID beside it and the time in the viewer's local time.

## A button inside Channel Setup was invisible (Layout Guard)

`.cs .acts` sets `opacity:0` for the tree's row buttons (shown on hover or select). The Layout Guard card sits inside `.cs`, so its "Fix on Overview" button was hidden too.

*Avoid:* a new card on a page with a wrapper class (`.cs`, `.wizbody` ...) checks the wrapper's rules for the class names it uses and scopes its own override (`.cs .lg .acts { opacity:1; }`). Look at a screenshot of every state, not only the default.

## The Overview page read a signal after its page was gone

The "page switch" e2e failed once with `Tried to access a reactive value that has already been disposed`. The Overview (score) page's `load` matched `Err(e) if all.get_untracked.is_none` after the config request, which reads `all` without the check from pitfall 2. A request that fails after the user left the page panics. The panic exists in any build; request timing decides if a sweep hits it.

*Avoid:* `if x.is_disposed { return; }` right after the `.await`, before any `match` arm or guard that reads a signal. A `match` guard (`Err(e) if sig.get_untracked...`) is a read too.

## "Add panel" on an empty ticket panel list made a bare panel message

With no panel saved, "Add panel" under "More buttons and forms" pushed one "New panel" button with empty message text, so #open-a-ticket showed only the title and that button. Only "Edit the ticket button" filled the default text.

*Avoid:* every control that makes the first item of something the bot posts brings the defaults that go with it, as the main start button does (`form.rs` panels: `start` and `add`).

## A design token name was already a dashboard variable

Design tokens are global custom properties (`--page`, `--line`, `--ok`, `--bad`, `--warn`, `--info`). `style.css` already had `--page` (the old gradient) and the four status names; the later file silently won. A type class from the generated tokens.css (`.small`) would have restyled every `.small` in the dashboard.

*Avoid:* colours, spacing, corners and font stacks come only from `design/tokens.css`, generated by `tools/design-tokens.mjs` (custom properties only, no `@font-face`, no type classes). `style.css` never defines a variable a token already names; its short names (`--b1`, `--fg`, `--primary` ...) only point at tokens. Before adding a token, grep `style.css` for `--<name>\b`. CI checks that `tokens.css` matches `tokens.json`.

## Animations always play; hover never lifts lines or scene pieces (front page)

Owner rule: animations are always on for the whole site; no motion switch, no saved choice, no rule that stops them, also under `prefers-reduced-motion`. `design/bundle.css` does not cut `tb-` animations; it only removes the 2 px hover lift.

The front page lifted on hover because the global `button:hover` rule (`style.css`) and `.card.link` moved the line buttons and scene pieces up. The shield outline looked thicker on one side because an inner shield cut with the same percent polygon is thinner on the slanted sides.

*Avoid:* on the front page a line only turns grey to white on hover; nothing moves (`.land .feat-tab`, `.land .scene` pieces). Check a hover rule written for all buttons on the front page. A shape outline is a stroke (mask), never a smaller copy of the shape.

## Every page had its own sizes

`style.css` had 25 font sizes, 16 corner radii and 56 paddings, and 14 pages set sizes inline with `style="..."`.

*Avoid:* a size comes from a token: text from `--fs-*` (generated into `design/tokens.css`, plus `--fs-figure` and `--fs-page-title-phone` at the top of `style.css`), padding from `--space-*`, corners from `--radius-*`. `0`, a percentage and `em` (relative to a token) are fine. A page never sets a fixed `style="..."`; it uses a class (`.grow`, `.fullrow`, `.vstack`, `.center` ...). A computed style (a colour, a position, a CSS variable) is fine. `tools/ui-sizes.mjs` fails CI otherwise. A new size goes into the design system first. `#/ui` shows every block and size: check a change there.

**Five text sizes:** `page-title` (44, 32 on a phone), `figure` (26), `section-title` (18: headings, card titles, crumb, brand), `ui`/`ui-strong` (14: body, buttons), `caption` (12: help lines, chips, table headers, code). The old 11, 13, 15, 16, 17 and 19 px tokens are gone from the Dashboard group; a rule that needs a size picks the nearest of the five. `tools/ui-sizes.mjs` fails on any other `var(--fs-*)` in `style.css`. The landing page types (`headline`, `lead`, `body`, `small`, ...) belong to the website part of the design system and are not touched.

**Page widths and rhythm:** the page column is `--w-wide` (1200 px, set on `main`; see "One page width" for the wide steps). There is no narrow column. Gap between blocks (header, tabs, cards, stack items) is 24 px, inside a card 20 px. `tools/ui-sizes.mjs` fails on a `max-width` of 800 px or more that is not `var(--w-wide)`. Dialogs, previews and fields smaller than that keep their own max-width.

**One header, one tab row, a crumb instead of "Go Back":** every page header is `Head` (`common.rs`); a detail page passes `up` so the crumb reads "Module › Parent › Page" (Filters sub pages, cases; the Permissions dialog shows its own `.crumbline`); every tab row is `PageTabs` with `Tab::link` (route) or `Tab::state` (page state). No own `<div class="head">`, no "Go Back" variants (ghost button, `.card .back`, `.casepage > .back`), no hand-built tabs. Tabs: a quiet row, an orange underline (`border-bottom-color:var(--primary)`) on the open tab, no pill box, never wraps, scrolls sideways. `tools/ui-sizes.mjs` fails on `class="head"` or `class="tabs"` outside `common.rs`, and when the `.tabs` rules lose the underline or get a fill; the e2e sweep checks the open tab's computed bottom border and background on every page with tabs. Channel Setup's `.seg` switch is a declared exception, not a `.tabs` row.

**Cards move up, no holes; one add button:** the settings grid must not be a CSS grid (a short card left a hole beside a tall one) and not CSS columns (they rebalance on every height change, so a growing card jumps columns under the cursor). `Cards` (`common.rs`) splits the list once into two fixed columns (first k left, rest right, balanced by rows per card via `split_at`); each column is a flex stack, one column below 900 px, same order as the list. Every add button is `AddButton` (green pill, circle plus, a text naming the thing); no green "+", ghost "+ Add", dashed wide box, `Empty` dashed button or grey dashed preview button. `tools/ui-sizes.mjs` fails if `.cards` becomes a grid, on a button text starting with "+" or `label="+` outside `common.rs`, and on a `dashed` button class. e2e selectors use `button.add:has-text(...)` (the plus is its own span, so `text-is` does not match).

**Text case:** field titles come from setting names, so `.row label::first-letter` makes them sentence case (not `capitalize`, which gave Title Case). Only chips are upper case; type button and heading text in sentence case ("Confirm", not "CONFIRM").

## A disabled button looked like a live one

The base `button` rule had no `:disabled` look, so Save with no change, "Fix issues" with nothing to fix and every permit-locked button looked pressable; only a few page rules (`.vitem`, `.chrow`, the wizard arrows) faded theirs.

*Avoid:* `button:disabled` is at half opacity with a not-allowed cursor (the design system's `.tb-act:disabled`), except a button that shows `<Working />`. A page rule for a disabled button only adds to that.

## Tab pages had no selected sidebar link

Raid Log, Join Gate Log, Raider Matches and similar tab pages have their own page key, and the sidebar compared keys with a list of special cases (appeals, reports, lockdown), so no sidebar link was selected on them.

*Avoid:* `nav_key` (main.rs) maps a tab page to its sidebar page, and the sidebar and the header breadcrumb both use it. A new tab page adds its key there.

## A five-column table squeezed into strips on a phone (Anti Nuke rule coverage)

The table scrolled sideways inside its card, but the columns still shrank first: notes were one or two words per line and "Exempt permits" sat past the screen edge.

*Avoid:* every table is `table.data`; on a phone each row turns into a block, so every cell after the first that shows a value needs `data-label` with its column name, or it shows with no label. A cell with no label (buttons, a detail row) takes the full width. A grid of cells (Channel Setup's Advanced grid) is `table.data matrix` and stays a scrolling table. `tools/ui-sizes.mjs` fails on a `<table>` without class `data`. The overflow test ignores text that is only scrolled out of a horizontal scroll strip (the wizard steps).

## A "+" let you add past the server's limit

"Add new action" (auto warn actions, max 5) and "Add snippet" (max 100) had no UI limit, so a sixth action was refused only at Save. Other lists copied the limit by hand (`100` messages, `7` schedules, `12` panels, `5` and `6` form fields), so a server change would leave the UI wrong.

*Avoid:* a "+" list takes its limit from the server: `/api/limits` (api.rs `limits`, from the constants the saves check) through `common::limit`, `room` and `<Count>`, or a `cap` the schema sends. Never type the number into the UI. A new limited list adds its key to `limits` and shows `<Count>` next to its "+"; an empty list shows `<Empty>` with a dashed "+ New ...".

## The sidebar lists modules, not pages

The sidebar has a few general links and one line per module; a module's parts are rows on its module page (`#/<server>/module/<slug>`). A page not in a NAV group has no row, and the e2e sweeps (`dashboard.js`, `deep.js`) find pages through the sidebar and module rows, so it would never be tested.

*Avoid:* a new page goes into a NAV group in `main.rs` (the header search, breadcrumb and module page read it) and gets a one-line `blurb` in `modules.rs`. A part with an on/off switch is added to `modules::flag` so the row shows "Set up" while it is off. Old links `#/<server>/<page>` keep working; never remove a route.

- **Reading `access` inside the route closure rebuilt the page on every access fetch.** `module_of(.., |k| access.of(k) ..)` ran inside the `move ||` that builds the page, so it tracked `access.pages`; access is fetched again on each page change, so the page was rebuilt a moment after it appeared and typed text was lost. The `page_access` memo exists to prevent this. *Avoid:* inside the route closure read access only through `page_access`; wrap any other access read in `untrack(..)`.

## A log type or event needs its category on the Customize list

The Logging "Customize" lists (Brim log types and Discord events) are folded categories built from each option's `group` (`kind_group`, `event_group` in `schema.rs`). An option without a group lands in a nameless category.

*Avoid:* a new log type goes into `LOG_KINDS` and gets a category in `kind_group`; a new `LogEvent` gets one in `event_group` (the same function feeds the per-event cards).

## A row list that reads the saved settings closes every open row (Commands)

The Commands rows are `<details>`; a list closure that read the per-command settings rebuilt every row on each save and folded the one being read.

*Avoid:* the list closure reads only what changes the rows themselves (command list, filter, whether the settings loaded). A row reads its own setting in a small closure (`entry("prefix")`), so a save changes two chips and nothing else. A button or tick box inside a `<summary>` stops the click (`stop_propagation`, plus `prevent_default` on a button) or the row opens with it. The settings are for the server last opened: the page names that server in the dialog and shows the controls only to its owners after the settings loaded.

## An error from a dialog's Save showed behind the dialog

The toast had `z-index:41` and a `.modal` has 50, so a refused save from a dialog that stays open (per-command settings) showed dimmed under the scrim and could not be clicked away.

*Avoid:* the toast is the top layer (`z-index:60`); nothing a page opens goes above it. Check a dialog that sends a request while it stays open with a refused save.

## The 2FA code must count for the server the page saves to

The 2FA prompt verified for the server in the address. Commands has no server there, so the code counted for no server and the save was refused again, without end. Reading `last_guild` from the browser store is wrong too: another tab can change it.

*Avoid:* a page without a server in its address that makes a 2FA-guarded save tells the prompt its server from the same source its save uses (`Access.pages.guild` for Commands).

The prompt also opened behind the settings dialog (same z-index, prompt first in the page), so the save looked dead. *Avoid:* a save made from inside a dialog needs the prompt above it (`.modal.top`); check in the browser with a login that has no fresh 2FA.

## The bot's captcha picture was missing from every preview

`discord.rs::embed` loads only `https://` images (so a typed address cannot learn the viewer's IP). The bot's captcha sample is a `data:image/png;base64,` address, so the picture was silently dropped in the wizard preview (Verification showed it only through a plain `<img>` fallback).

*Avoid:* the preview's image filter also lets `data:image/png;base64,` through (it makes no request). A new sample with an embedded picture uses that form, never another `data:` type. Preview the real message: a missing part of a bot sample is a filter, not a missing sample.

## A switch in the sidebar is no weaker way in

The open module's On/Off switch in the sidebar (`modules::ModuleSwitch`) saves with the same `PUT /config/<section>` as the General page, so the server's access and 2FA rules apply. It sends the module's whole stored section with `enabled` flipped, never a partial object (the PUT replaces the section).

*Avoid:* a new module switch uses `switch_section` and `ModuleSwitch`; never add a separate route. The switch sits inside the module's link, so its click calls `prevent_default` and `stop_propagation`, and the checkbox follows only the saved config. Turning off asks first (`ask` with `.danger`); the toast, busy flag and 2FA state live in `App`, because the nav is rebuilt when the config changes.

## The landing page showed for a moment after every login

`App` drew the landing page as the fallback while `me` was empty, and `me` stays empty until `/api/me` answers. After the login callback sent the browser to `/`, the landing page showed first, then the server list.

*Avoid:* until `/api/me` has answered, `App` shows only the loading sign (`checked`). Set `me` before `checked`, or the landing page shows between the two sets. Any new "logged in or not" view waits for that answer too.

## After the 2FA code, some actions asked to be pressed again

The prompt repeated an action by clicking the control the click landed on again. That fails when (1) the request is sent after an await: no click to repeat (the click record is cleared at the end of the click). Overview Fix all sends fixes one after another, so a step-up that ran out mid-run opened the prompt with nothing to resume, the toast said "press the button again", and a second press ran the whole list again from a list read before the run. Config import (after the file read) never resumed. (2) The control is gone or drawn again: the Apply import button, Channel Setup push dialog, Delete server data dialog and Load backup dialog close before their request; the panic mode button shows the other action.

*Avoid:* a request that starts after an await, or whose control closes, hides or changes, uses `run_resumable` (or `run_change_resumable`): it gets a closure that builds the same request again, and the prompt runs it once after a good code (dropped on Cancel and on a page change). A run of several requests keeps what is left and goes on from there. A plain click handler that keeps its control on the page can stay on `run_act`. The wizard card's setup request (`form.rs` `act`, also sent after a save) is resumable too. A refusal that comes back after the user opened another page opens no prompt. Where nothing can be resumed (the Channel Setup draft auto-save puts the server's draft back on a refusal), the prompt says "Press the button again to finish", never a bare "Verified.". The server checks the step-up and CSRF token on the resumed request as on any other; nothing is stored outside the page's memory.

- **Server card without banner.** Discord sends `banner` in the guild list only for servers with a banner (boost level 2); most have none, so cards showed an empty gradient. *Avoid:* fall back to the zoomed, blurred icon (`overflow:hidden` on the parent). A button in a flex row needs `flex:none; white-space:nowrap`, or a long name wraps it.

## A grey frame around the Backups settings

The Backups page wraps its settings in a `<fieldset>` so a viewer below owner level gets them read-only. Only `fieldset.ro` had border and padding removed, so for the owner the browser's default fieldset frame (2 px groove, padding) drew a box around both cards. The e2e sweep did not catch it; only a desktop screenshot did.

*Avoid:* a plain HTML element that the browser styles (fieldset, legend, details, button) gets its base reset in `style.css` once, for every state, not only in the class one state adds. `fieldset` has no frame by default; the e2e sweep fails on a framed fieldset on Backups.

## A detail page kept its own big title and back button (Backups)

One backup opened inside the Backups page from a signal: the page drew a second `h1` (the backup code) and a "Go back" button under the page header. A detail held in page state cannot reach the crumb, which `main.rs` builds from the route.

*Avoid:* a detail view is a route (`#/<server>/backups/<code>`, `#/<server>/cases/<no>`), and `main.rs` adds it to the `detail` crumb list; leaving it is the crumb link, and a delete goes back with `set_hash`.

## Everything looked tiny on a big monitor

Fixed pixel sizes made a 2560 px monitor show a small column of 14 px text.

*Avoid:* the screen scale is one `zoom` on `#app` (`--z` set on `:root`), stepped by window width through `--z` (`design/README.md`, "Screen scale"). Chrome and Firefox do not zoom `vw` and `vh`, so a bare `100vh` is `--z` times too tall; use `calc(N * var(--vh))` / `var(--vw)`. Window coordinates (`getBoundingClientRect`, `client_x`, `scroll_y`, `inner_width`) are real pixels, while a `left`, `top` or `--dx` set from them is a zoomed length: divide by `screen_zoom` (`common.rs`) or the popup lands `--z` times too far (tooltips, the message editor's emoji and variable menus). Ratios inside one rectangle (the colour picker) are fine. `tools/ui-sizes.mjs` fails on a bare `vw`/`vh` and on a second `zoom` in `style.css`.

## Each list drew its own hover and drag look (sortable list)

The saved messages list had an orange hover outline, Channel Setup a background only, Permits up and down buttons: three looks for one idea, and orange on hover said "selected" when nothing was. Native HTML drag does nothing on a touch screen, so a whole-row drag cannot be the phone way.

*Avoid:* a sortable or selectable row takes its outlines from `--ol-hover` (grey) and `--ol-pick` (orange) in `style.css`; a new sortable list uses `.slist` / `.srow` with `Grip` from `common.rs` (mouse: native drag on the row; finger: pointer events on the grip only). A confirm dialog opened from another dialog comes after it in the page, or it opens under it.

## The Add feed dialog: both choices orange, a cut-off help bubble, a stray line (Feeds)

1. **Both source buttons were orange.** `.seg` (the two-choice selector) had no rule of its own; every page scoped it (`.cs .seg`, `.mcard .seg`, `.pop .seg`, `.send .seg`, `.chpick .seg`), so Feeds (with its sort control too) got plain orange buttons and no "open" state. *Avoid:* `.seg` has a base rule in `style.css` (muted buttons, the open one orange); a scoped rule only adds to it.
2. **A help bubble was cut at the dialog's edge, with a thin line along its bottom.** `.modal .card` scrolls (`overflow:auto`), so an absolutely placed `.tip .bubble` is clipped by the card and widens its scroll area (the line is the horizontal scrollbar). `Tip` placed the bubble inside `main`, which is wider than a dialog. *Avoid:* `Tip` measures against the dialog card when it sits in one (sideways and upward), else the page area. Every `Tip` in a dialog (Feeds, Commands settings, Permits, Channel Setup push) uses it.
3. **A grey "Type the YouTube channel." line read like an error and moved the buttons.** *Avoid:* say a missing field once, on the field's own hint line and on the disabled button's tooltip, never as a line between the form and its buttons.

**Also:** the add form asks for one link. `guess_kind` (`feeds.rs`) names the source from it (YouTube @handle, channel id or youtube.com page; github.com; gitlab.com; else RSS). YouTube's own `/feeds/` links are RSS. A self-hosted GitLab cannot be told from a website, so "More options" keeps a Source list (Detect from the link, RSS or Atom, YouTube, GitHub, GitLab). The server still gets an explicit `kind`, so no safety rule changed.

## Fields that a password manager takes for a login

1Password (and LastPass, Bitwarden, Dashlane) guess a login form from field names, search boxes and code fields, even with no password field. It offered "Sign in" on most pages (Bot Profile has a "Bot name" field and a search box). The dashboard has no login form: sign-in is Discord OAuth.

*Avoid:* do nothing per field. `index.html` `quiet` gives every mounted `input` and `textarea` `autocomplete="off"` (unless it sets its own) plus `data-1p-ignore`, `data-lpignore`, `data-bwignore` and `data-form-type="other"`. The authenticator code field keeps `autocomplete="one-time-code"`. Never add a `type="password"` field. The Rescue Key is shown as text and claimed in Discord, so no dashboard field holds it. The e2e sweep fails on any field without those attributes; the real test is a person with the manager installed.

## Channel Setup could not move a channel on a phone

On a touch screen Channel Setup turns native drag off, and the ⋮ menu's "Move to category…" only selected the row and called `focus` on the category picker in the side panel one animation frame later. iOS Safari ignores `focus` that does not run inside the tap itself, and on a phone the panel sits under the whole list, so the tap did nothing visible. The Roles tab had no finger way to reorder at all.

*Avoid:* a menu item does its job itself; it never only moves focus to a control elsewhere. The ⋮ menu holds the category picker (`<select>` opens from the tap). A list with a selected row (Channel Setup channels and roles) uses `HoldDrag` (`common.rs`) for the finger: tap selects, a press held still for 0.4 s on the selected row lifts it, a finger that moves first scrolls. Its places carry `data-drop`. A menu on one of the last rows opens upwards (`.rowmenu.up`), or the push bar covers it.

## A refresh button must refresh what the person is looking at

The sidebar's "refresh server" button only fetched `/api/guilds` again; nothing visible changed and the open page never refetched. It was also a green pill set absolutely inside the server link (`<a><button>`), which is invalid HTML.

*Avoid:* a refresh control refetches the data on screen (main.rs `refresh`: the server list, then `page_tick` builds the open page anew) and asks first when a save bar (`main .bar`) shows unsaved edits. A failure shows in the toast. Never put a button inside a link: put it next to the link in a row (`.srv-row`). Check in the browser: after the tap, more `/api/` requests than the server list must go out, and the page content must load again.

## A button that sends a request twice on a double click

A button whose handler starts a request (a queued job, a post, a key, a token) but never goes busy. A confirm dialog does not help: it runs its action once, but the first button is pressable again at once and opens it again. Found in recovery.rs, pages.rs, tickets.rs, lockdown.rs, secure.rs, main.rs (history), layout.rs, embeds.rs (Make channel), records.rs, mcp.rs, raiders.rs, appeals.rs, datareq.rs. Server side (not fixed): the handlers are not idempotent; a repeat queues a second backup or load, mints a second Rescue Key (the first is dead), posts a second ticket panel or makes a second MCP token. Purge, Secure my account and the decisions stop themselves on the second try. Only a script or a second tab can send the repeat now.

*Avoid:* wrap the request in `guard(busy, fut)` (common.rs; sets `busy` at once, clears it when the request answers, also on a refusal or a 2FA prompt so the retry can click again), then `disabled=busy` and `busy_label(..)` on the button. A list row uses a signal made in its own closure (or a `(id, key)` pair for the row's several buttons). A queued bot job (`recovery.rs queued`) keeps `busy` on until its reload, so the same job is not queued twice before the bot has taken it. A new button that starts a request needs `disabled=busy`; click it twice fast in the browser and count the requests.
## A slow action needs a pending state
- **A slow button showed nothing for 2-3 s** (`update.rs`, the server asks GitHub first). *Avoid:* use `busy_label`; set busy before the request so the button shows the working sign and is disabled from the click. Clear it on a refusal (a 2FA retry can click); keep it until the next answer shows the queued run.
- **A failed poll while the dashboard restarts itself is expected.** *Avoid:* show the same page (text only), never a pose. A pose is a state of its own (empty, error, offline), not a loading flicker.

## Screen scale stops at 1920
- **`zoom` above 1.25 made a 2560 px window look zoomed in.** *Avoid:* the scale stops at 1.25 from 1920 px up; more width is margin. `tools/ui-sizes.mjs` fails on a `--z` above 1.25.
- **Wide windows.** Above 1920 px the width goes to columns: `--w-wide` widens (1600 px from 2300, up to 2300 px from 3200) and `.cards` becomes CSS columns (3, then 4). A card that grows can push the next card into the next column; by design, wide screens only. *Avoid:* never put a page with its own grid under `.cards`.
- **A label with its field beside it outside a `.row`** needs the `field` class, or the text touches the input.

## Signals after an await
- **`try_set` returns `Some` only when the signal is gone.** "Page gone, stop" is `if busy.try_set(false).is_some() { return }`. Feeds had `is_none()`: each action stopped on success (dialog stayed open, no reload, no message). `try_update` is the other way round: `Some` means it ran.
- **A signal made in a click handler belongs to the view that drew the button.** When that view draws again (names arrive, a list reloads) the signal is gone while a dialog still reads it: panic. *Avoid:* make the dialog's draft signal once in the component and set its value on open (`feeds.rs` `open_dialog`, `open_editing` of Sticky, Auto Responder and Triggers).

## Wide columns, loading names, mention text
- **Home cards touched in 3 and 4 columns.** The later `.home .card { margin:0 }` has the same weight as `.cards .card` and won. *Avoid:* the shared rule uses `main .cards .card`.
- **A form drawn before `/discord` answered showed every set channel and role as "none"** (reads as "not set"). *Avoid:* load the name list before the fields; a set id missing from the list shows "not found (deleted, or not loaded)", never "none"; a failed name list shows an error.
- **Mention text from the server** (`<#id>`, `<@&id>`) shows names from `/discord`, never raw ids (Lockdown check). Load the names before the list.
- **A request with no error branch left its loading line forever** (Home Overview card on a failed score). *Avoid:* every loading state needs an error state.
- **`md::inline` does not draw `{e:name}` tags.** Embed titles and field names in previews need `icons::fill_tags` too.
- **A class used for two layouts broke the second** (`.facts` as a `dl` grid and as the Details/stat row pushed Overview Details off a phone). *Avoid:* scope by element (`dl.facts`).
- **"Update now" showed when the newest build was unknown.** *Avoid:* a button shows only when its action can work; Update now needs a known newer build (the server refuses such a start too).

## Feed rows
- **Row buttons moved with the text.** `<span class="sp">` grew only inside parents with their own `X .sp { flex:1 }` rule; `.frow` had none, so Edit and ⋯ jumped when the status line changed. *Avoid:* `.sp { flex:1 }` is one global rule; a spacer works in every flex row. Controls sit after the `.sp`; the text block has `min-width:0`. On a phone the row is a grid (owner's layout): icon, title (with chip, in `.ftitle`) and switch on the top line, details and error lines over the whole width (`.fmain` is `display:contents` there), ⋯ and Edit on the last line at the right. A `<details>` in a row needs `margin:0`.
- **An untouched default showed "{link} is missing".** `botmsgs::missing` read only the first embed's text, but the GitHub look keeps `{link}` as the title's link (`url`), which the server allows for `feed.*`. *Avoid:* the UI check follows `botmsg::missing_text` exactly; `every_default_passes_its_own_check` and `a_feed_link_may_be_the_title_link` prove it.
- **"last post in a minute ago".** `relative()` adds "in" or "ago" itself. *Avoid:* pass `time - now`; a past time is clamped below 0 so a slow clock never reads "in".
- **"Asked" stayed on a row**; a webhook feed turned on kept `asked = 'check'` for ever. *Avoid:* the page reads the list again every 10 s while a feed waits; the server sets a check only on feeds it reads and hides `asked` on webhook rows.

## Channel Setup list is compact; set-up buttons
- **Compact list (owner rule).** Its row rules (lines starting `.cs .tr`, `.cs .slot` or `.cs .nm` in `style.css`) do not follow the 4 px scale or padding tokens; `tools/ui-sizes.mjs` skips only those lines (`CS_LIST`). *Avoid:* do not make these rows larger. Other Channel Setup rules (panel, dialogs, toolbar) still follow the scale.
- **Rows were 44 px high** because global `details { margin:12px 0 }` hit the "⋮" menu (`details.rowmenu`). *Avoid:* a `details` used as a small control in a row needs `margin:0`.
- **A dashed line showed between groups:** the embed image `.slot` rule also matched Channel Setup's drop slots. *Avoid:* a slot shows only while a drag runs (`.cs .slot:not(.on)` resets it); a short shared class name needs a scope.
- **A "Set up" or "Create" button shows only when its action has something to make:** Catch Channel hides "Set up channel" while the catch channel is set and exists in Discord; Logging hides "Create a channel per log type" while every log type has a channel (`GET /logging/setup` gives `todo`, from the same `log_gaps` the setup uses); a wizard card's action hides while its `done` channel exists. Tickets already did this. *Avoid:* a new set-up button follows the same rule.

## One page width
**What went wrong:** three widths. Cards pages (and Channel Setup) widened through `main:has(.cards, .gcards, .cs)` from 2300 px; others stayed at 1200; lists, `details.how`, `.mcard`, `.feeds`, `.stack` used a 900 px `--w-narrow`. A page jumped wider when its cards loaded, and "How it works" was narrower than the cards.
**Rule (owner):** the content width is one token, `--w-wide`, set only on `:root` and stepped only by window width (1200 px, 1600 px from 2300, up to 2300 px from 3200; all at scale `--z`). `main` uses it; every block inside a page fills `main`. Never set the width from page content (`:has(...)`); never give a page-level block its own `max-width`. Only dialogs, previews, fields and small forms inside a block may be narrower. The server list has no sidebar, so it is wider than server pages below 1500 px; same token, not a second width.
**Check:** at 1366, 1920 and 2560 every server page's `main` content box has the same width and its direct children fill it, also while loading.

## The small sidebar switch rule never applied
- **The full 44 x 24 px switch showed in the sidebar** (row 3 px higher): `nav a .sw` and `.atit .sw` (36 x 20 px) lose to `input[type=checkbox].sw` on specificity. *Avoid:* a rule that changes the switch size uses `nav a input[type=checkbox].sw`. Measure the row heights: every sidebar module row has the same height, with or without a switch.
- Every module with a single on/off (Auto Mod, Anti Nuke, Tickets) shows its switch in the sidebar, open or not; a part with its own on/off shows it on the module page. *Avoid:* tests select a switch by its `aria-label` ("Anti Nuke on or off"), not by `nav input.sw`.

- **An extra panel went live as a "New panel" button** (tickets). "Add panel" gave every panel after the first the placeholder label "New panel", no emoji; members saw it in #open-a-ticket. *Avoid:* a default that the bot posts is text members can read as is ("Open a ticket"), never a placeholder. New panels carry no category, so their tickets go to the ticket category. Migration 4057 gives a saved panel with the placeholder ("New panel", no emoji) the default button and drops its category; the bot posts the panel message again at start.

## The front page top bar never showed on a short screen
The bar is hidden until the visitor scrolls past the hero's copper line. On 1366x768 the page ends 418 px below the top, so the line stays 350 px down and "the line has left the top" never fired. *Avoid:* a scroll trigger that depends on a position first checks that the page can scroll that far. `landing.rs` shows the bar at the line, or at half of the scroll range when the line is out of reach. A hidden bar uses `visibility:hidden` (not focusable); never opacity alone.

## The front page bar broke the size check
The bar rule wrote `padding:... calc((100% - 992px) / 2)`; `tools/ui-sizes.mjs` allows no raw pixel value in a padding, so dashboard-ui failed on main. *Avoid:* a width you need inside a padding is `--w-wide` (never a raw value or a new property that only hides it). Run `node tools/ui-sizes.mjs` before every push that touches `style.css`.

## Module and part switches lied after a failed reload and on an off module
- After a good save the switch read the config again and ignored a failed read: old state kept, toast said "Saved!".
- A part of an off module (Join Gate while Server Joins is off) showed ON and asked "Turn Join Gate off?", but nothing ran. An Editor got an enabled part switch there and every click was refused, because turning the module on needs a Trusted Admin with 2FA.
- At 375 px a part card held icon, text, switch and chevron; the title wrapped in an 80 px column and the chevron left the card.
- Root cause: switches tested only on the good path, as Owner, at desktop width.
- *Avoid:* a control that reads the state again after a save reports a failed read as an error. A part switch shows what runs (`part_on` reads the module too), and its `may` asks for every bar the server checks for that write. Measure a card row that adds a control at 375 px.

## A dead session, a failed load and level none showed false pages
- A session that ended while a page was open kept the signed-in shell (Cases: "login required" next to "No cases found.", no way back to sign-in).
- Six list pages set `loaded` after a failed load too, so empty text ("No cases found.", "No backups yet.", "No open tickets.") showed under the error as real data.
- A failed access answer showed a Viewer every page (counted as Owner).
- `/api/me` failing (500, no connection) showed a signed-in person the public front page. An access answer of 200 with no body left the skeleton. A signed-in member with no level got Home with "Open the wizard".
- Root cause: pages tested only with a live session and good answers.
- *Avoid:* `reply` sends a 401 on a signed-in page (CSRF set) to sign-in with `next`; logout and Secure my account call `signed_out()` first, so a late 401 does not sign the person in again. A list sets `loaded` only after a good load. Only "login required" from `/api/me` is the front page; other errors say so. An access answer counts only with its `guild`; a failed one fails closed (`failed`: no page, rank 0, a Try again card). Level `none` sees no page and no Home.

## Leaving with unsaved edits
- One guard for the dashboard: `guard::guard(dirty, busy, problem, save)` in the page, `guard::leave(then)` for a page's own Back button. Never write a page-local "Leave without saving?" ask. `problem` is the text of what must be fixed first (a missing `{text}`, a JSON error); the dialog then has no Save button.
- A page change is held in `guard::hold` (main.rs router): the address is put back with `replaceState`, the dialog opens one tick later (it closes itself on a hash change). Do not create an `Effect` in a dialog answer: the dialog's view owns it and drops it when the dialog closes; wait with a timeout loop (`settle`).
- Tickets and Logging are hubs like the other modules: parts are NAV keys like `tickets/publish`; `Access::of` reads the part before the `/`.

## One searchable picker for channels, roles, categories and members
- Choose a channel, role or category with `form::ListPick`, never a native `<select>`, a plain text or ID box, or a `<datalist>`. Many of them are chips plus the same `ListPick` (`channels`/`categories`/`roles` kinds). Choose a member with `UserPick` (`form::one_member` for one). "Pick, then Add" controls keep the pick in a signal, not a `NodeRef` to a select.
- Root cause of the empty Channel box on Publish panels: `ListPick` was an `<input list>` with a native datalist. Firefox shows no list on focus or click, only after a key, and no arrow, so it looked like an empty text box; Chrome opens it, so tests missed it. `ListPick` now draws its own list (opens on focus, filters, arrow keys, Enter, Escape) because browsers differ on native lists.
- *Avoid:* a new field for a channel, role, category or member uses these pickers. `tools/ui-sizes.mjs` fails on a datalist input (only the time zone box in `msgrules.rs` may keep one) and on a `<select>` for a channel, role or category. `tests/e2e/dashboard.js` opens the Publish panels picker, filters it and clears it.

## The picker list was cut off inside a dialog
`ListPick`'s open list was `position:absolute`; a dialog (`.modal`, `overflow:auto`) or a card with hidden overflow clips or scrolls it (Feeds "Change" dialog showed three rows). *Avoid:* the list is `position:fixed` (`.lpl`), placed from the box's rectangle every animation frame while open. It opens below, or above when less than 160 px is below; height capped to the room on screen; it scrolls inside (`overscroll-behavior:contain`). The page is scaled by `--z` zoom on the root, so offsets are divided by the ratio of the rectangle width to `offsetWidth`. Escape closes only the list while open. A new popup list uses the same pattern, never `position:absolute` inside a dialog. `tests/e2e/dashboard.js` opens the Feeds dialog picker at desktop and phone size and checks the list is inside the window, on top and next to its box.

## The release browser test waited for a page the leave guard held
`tests/e2e/dashboard.js` edited the Verification page, then `page.goto` to Messages: only a hash change, so `guard::hold` put the address back and asked "Unsaved changes"; "Add new message" timed out. The e2e job runs only on a tag, so main stayed green. *Avoid:* a step in `dashboard.js` that leaves an edited page goes through the shared `page.goto` wrapper, which answers "Discard changes". A new page guard needs no test change; a page that keeps edits on purpose is tested with its own click.

## Front page: switching a line moved the page
Each scene set the stage height (`--sh`) when active, so the block height changed and the page below jumped. A hover lift on the pieces and a bold active line made it feel loose too. *Avoid:* the stage is as tall as the tallest scene of its block; a switch changes only which scene shows. Do not change the weight, size or height of anything a click on a line touches. Do not use `prefers-reduced-motion` to turn off front page motion: motion is always on (owner rule).

## Front page: stars placed against one scene covered another
Stars placed against the open scene's group covered text in other scenes, because scenes of different size sat at different places in the shared box. *Avoid:* the stars are fixed in the block's box. All scenes of a block start at the same corner; star places come from the sizes of the first pieces of all scenes (`fit` in `landing.rs`). After a scene layout change, open every scene of every block at desktop and phone width and check no star covers text and no scene leaves the box.

## Icons on coloured buttons lost their contrast
Emoji pictures have an orange gradient (`fill="url(#name-grad)"`, `stroke` the same); the dashboard draws the same SVG through `icon`, so green Load, red Delete (backup page) and the wizard's green and red arrows showed orange on green and red. *Avoid:* on a filled button an icon takes the button's text colour. `style.css` sets `fill` and `stroke` to `currentColor` for the gradient parts under `button.go`, `button.danger`, `a.btn` (not ghost) and `.wiz .nav`, as the active menu link does. A new filled button style with an icon joins that list.

## A password manager's button sat far from the 2FA code field
The screen scale was a `zoom` on the page root. A password manager places its button from the field's `getBoundingClientRect` (already scaled), and the button sits under the zoomed root too, so the place was scaled twice (at 1.25 about 25% too far right and down; Firefox and Chromium). *Avoid:* the zoom sits on `#app` in `index.html` (the app mounts there, last in body), never on `html` or `body`, so anything a browser extension adds keeps real pixels. Check that a button placed from a dialog field's rectangle lands on it.

## A question box opened behind the dialog that asked it
`ask()` (common.rs) used the same z-index as page dialogs and comes earlier in the page, so a dialog that opened it covered it ("Save as template" showed no name box; the page looked frozen). *Avoid:* the box of `ask()` has the class `modal top` (z-index 55, above dialogs, under the toast). A new overlay that can open from inside a dialog uses `top` too. Run the flow in a browser to the end: a click that works in a test with force can hide a box nobody can see.

## A call to the list URL ended with a slash
`act(String::new(), ..)` built `{base}/`; the server routes list and start on `{base}` and answered 405, so Start from a form did nothing. *Avoid:* build an API URL from parts with one helper that never leaves a trailing slash (empty path: no slash); let the browser test press the button that starts the thing.

## A long variable chip ran out of the editor box
A chip had `white-space:nowrap` and no width limit; a long name pushed it under the next column, and on a phone it was cut at the screen edge. *Avoid:* an inline part that cannot wrap needs `max-width:100%` and an ellipsis (full text as title). Check in the narrowest column and at 390 px.

## A preview drew only the part that can be edited
The preview lacked the buttons Brim adds when it posts (Enter, Claim, links under logs), and a wrong variable example (names where a running giveaway has a number) went unseen. *Avoid:* a preview shows the whole message a member gets. Parts Brim adds come from the bot's own builders (`crates/bot/src/previews.rs`, `added_buttons`), never from a copy in the page. They carry the shared notice and cannot be pressed. Give a variable the example that fits each message.

## Revert and Undo set the message without the parts Brim adds
Revert replaced the whole editor value with `{"body": ..}`, losing the `added` rows (buttons Brim adds), so the preview showed less than the bot posts until reload. *Avoid:* change one field of the editor value (`cur.update(|c| c["body"] = ..)`), never rebuild the whole value in a later path.

## One example value for a variable that means different things
`{text}` had one example ("What happened, written by Brim.") in every message; in a ticket reply it read as a member's words. *Avoid:* when messages fill a shared variable name with different things, give each its own example (`var_info` in `crates/logging/src/botmsg.rs`), taken from the server's real data where the dashboard knows it (the ticket panel lines).

## Click-to-edit in the preview must not become a second editor
A click on a part of the editor's own preview opens the editor's text box or button popup for that part; it holds no data. `discord::bot` marks parts only when the message carries `_edit` and each embed `_i` (its place in the editor's message, since colour-only embeds are left out of the preview). Other previews carry neither and stay plain. *Avoid:* the part's address (`data-p`, `data-b`) is the editor's own JSON pointer or row:place, checked to plain path characters before it goes into a selector. Links in the text, spoilers and menus keep their Discord behaviour. From 940 px card width the phone preview shows beside the desktop one (notice only once); `e2e` selectors on the preview use `.first()` where the editor is closed.

## A long link in a Discord preview ran past the message box
A long link had no break point and ran outside the box on a phone. *Avoid:* the text column of `.dmsg` has `overflow-wrap:anywhere`. Check at 390 px with a long link.

## The browser's own undo does not know the message
Ctrl+Z in a text box only undoes typing; an added field, changed colour or removed embed changes the message, not the box. A box drawn again (another message loaded) also lost its undo steps. *Avoid:* undo and redo go through `Editor::step` in `embeds.rs`; keys and the browser's own undo (`beforeinput`) both end there. A place that loads another message into the editor calls `Editor::reset`, or Undo brings back the old message. Leave Alt out of shortcut checks: AltGr is Ctrl+Alt on many keyboards.

## The tab row has one sliding bar
`PageTabs` (`common.rs`) measures the open tab and sets `--tx` and `--tw` on the row. `.tabs::after` is one orange bar that moves with `transform` only; content under the row fades in with opacity (`tf-a`, `tf-b`). Without the measure the open tab keeps its own orange border. *Avoid:* no second underline on a tab row; do not animate left, width or height there. The row stays while its tabs switch (see "A route tab built the whole page again"), so it slides from its own last place; a row built new puts the bar in place with no motion (`data-still`). A test of the orange line reads the bar when the row has `data-slide` (`tests/e2e/dashboard.js`).

## A glow and a clip-path on one element
The star glow was a `filter` on the same element as the star `clip-path`; the clip runs after the filter and cut the glow. The star shape is now on `::before`, the glow on the element. The big buttons' hover line was a layer behind the button (`::before`, `inset` -3 px, radius plus 3 px) with the button covering its middle; a ring made with a mask beside the button left a gap and a hairline. *Avoid:* to light the edge of a shape, put the shape on a child and the filter on the parent.

## Hover and focus lines come from the element's own geometry
A hover line, ring or highlight is a layer ON the element it marks, never a separate shape beside it.
- Wrong: a second box, mask or border next to the button with its own size and radius (drifts, gap or hairline, not concentric).
- Right: `position:relative` on the element and a `::before` with `inset:-<width>`, `border-radius:calc(<the element's radius> + <width>)`, `z-index:-1`. The element covers the middle, so the line is an even edge. Take the radius from the same token as the element.
- A hover line must not change layout: use `inset`, no border or margin.

Two causes of a clipped ring:
1. A parent with `overflow` (`main { overflow-x:clip }`, `nav { overflow:auto }`) cuts a ring drawn outside. Give the parent `overflow:visible` where safe, or draw the ring inside with a negative `outline-offset`.
2. `clip-path` on the element cuts its own `filter` glow (see above).

*Avoid:* after any change to a button, Tab through the page and check each focus ring is complete on all sides.

## Front page: the switch ended with a jump
The new scene sat a little right while it moved, with no real fade, and jumped when it stopped (paused at 299.9 ms of 300 ms: about 20,000 pixels differed from the finished page, text about 1 px off). Cause: a scene is placed with `translate` and `scale` set to fractions (fit values `--dx`, `--k`). While animating it was its own layer at a fractional position; when the motion ended the browser dropped the layer and the text snapped to a pixel. `getBoundingClientRect` showed no difference, so a position check passed. The fade was `ease-out` (new scene at 74% in the first 40% of the time), so the cross-fade looked like a pop. *Avoid:* a motion ends exactly at the resting layout. An animated piece keeps its layer when it rests (`will-change:transform,opacity` on `.feat-shot > .scene` and `.fx`), so the first resting frame is the last animated frame. (The even cross-fade is replaced by a fast fade out, then an ease-out rise; see "A front page scene switch showed a double image".) Check a motion by recording (pause at 0, 25, 50, 75 and 99.9% and compare; the page at 99.9% must equal the rested page), never from still screenshots or rectangle numbers alone.
Owner rule: the big front page buttons show nothing on hover or when pressed except the 97 % press-down; the `::before` layer is gone. Do not add a hover or pressed highlight to them again. The rule above still holds for any other outline.

## A pressed button turned white under its label (front page)
The white layer of a pressed button (`::before`, `z-index:-1`) lay over the button's own fill, because the press transform makes the button a stacking context; light labels became white on white. Fixed by removing the layer. *Avoid:* a state that changes a button's fill sets the label colour with it. Look at the pressed, hover and focus still of every button style in both browsers before reporting.

## A list that redraws on every change
**After a keyboard move the open categories closed and focus was lost.** The page's `list` closure read the row count, so each change of the rows built the list component again, with new state. *Avoid:* read only what decides the layout (a `Memo` of "any rows"); keep the screen reader's live region outside the redrawn part; after a move put the focus back on the moved row (`ui/src/embeds.rs`, `ui/src/msgcats.rs`).

## A copy of a shared part
**The saved message categories got their own copy of the Default messages group opener** (same markup and classes, plus a grip and labeled pill buttons), so the owner saw a new design for a job that already had one. *Avoid:* before a new row, opener, list or button, find the page that already does the job and use its component; if it is a local closure there, move it to `common.rs` first (as `HubRow`) and use it on both pages. Extra controls on a shared part stay as quiet as the page's existing ones (icon buttons with a title, like the message row actions). A list that reuses another page's look takes all of it: same left edge, same indent (`.hubsubs`), nothing added beside the rows (no grip column, no line of buttons under the open row); extra controls go inside the row where that page puts its actions.

## The remove × of a chip dropped to a second line
In a picked list (Verification roles, channels, aliases) the chip is a flex item beside the search box; the row shrank it until the × wrapped under the name. The × button also carries the class `link`, a flex row meant for card links. *Avoid:* a chip in a picked list keeps name and × on one line (`.pick > .chip`, `flex:none`, `white-space:nowrap`), and the × is inline (`.chip > .link`). Every chip with a remove button uses these (roles, channels, aliases, giveaway lists, extra entries). Check a part that shares a flex row with an input with a still of the real page.

## A front page star sat on top of text
`fit` clamped a star's left place to 0 on a desktop. In a block whose widest scene fills the box (verification, and the dashboard-cards block at 1280 px and wider) the star's place is left of the box, so the clamp pushed it onto the first piece's text. The old check only looked at the first scene's sizes. *Avoid:* a star keeps its place outside the group; only a phone clamps it into the box. Check with a script that lists, for every block, scene and width from 820 to 2560 px, the stars whose rectangle lies over any text of the scene or the block's copy (it must list none), not by eye on one still.

## A front page scene switch showed a double image
The switch was a cross-fade (old scene fades out while moving down 48 px, new one fades in over it); for about half of it both were half visible, two sets of text and two orange labels showing through each other. A check on single frames in a headless browser found the motion "as written". *Avoid:* a switch between two pieces of content in one place never shows both readable: the old one leaves fast (0.1 s), the new one starts when the old is gone and settles with an ease-out (a linear end reads as a snap). Check motion in a recording from a real browser at the owner's window size (about 1070 px wide, also 1920 and 390) and with a slow CPU, beside the reference when there is one; look at the recording, not one still.

## Home cards did not sit on a grid
Home cards used the shared `Cards` columns (flex stacks, CSS columns from 2300 px); each column stacks on its own height, so a taller card (Overview with its ring, Message Builder with two text lines) pushed cards under it down and rows did not line up. *Avoid:* a page of fixed-size cards that must line up in rows uses a real grid (`.home-grid`: 1 column, 2 from 900 px, 3 from 2300 px, rows as tall as the tallest card, the button at the bottom of the card). `.cards` stays the columns flow for settings pages whose cards grow; it must never become a grid (`tools/ui-sizes.mjs`). Check card tops and bottoms per row at 2300, 1920, 1366 and 390 px.

## A route tab built the whole page again (2026-11-25)

**A tab that changes the address (Tickets, Logging, Lockdown, Appeals, Raids, Join Gate, Raider List, Reports, Whitelist, Anti Spam) blanked the page, faded it in twice and moved the underline late; on Logging both tab rows jumped 43 px.** The router's one closure followed the whole address, so each tab click built the header, the tab row and the page again; the page loaded its data again (the Logging setup button was gone until then), and `Tabs` built its row again on every access answer, which faded the content a second time.
**Fix:** the router builds from three keys (`frame_of`, `body_of`, `top_key` in `main.rs`): the header follows the address, a route tab row (`tab_group`) stays for all its pages, and the body changes only when its own page changes. Tickets and Logging take their tab as a signal and stay. Tickets keeps every part mounted, so its tab switch does not ask about unsaved edits and its Cases and Stats load again each time their tab opens (`open`), keeping the last list in place meanwhile; Logging builds its one form again for a new part, so it still asks. `Tabs` reads the access through a memo.
**Rule:** a page that switches parts by the address takes the part as a signal; do not put it in the router's key. Signals that a list or effect makes for later runs belong to an outer owner (`eased` in `common.rs`): an effect run drops what its run before made, and a later read panics with "already been disposed".

## A row under a switch moved the page at once (2026-11-25)

**A switch that turns a part off showed or hid the "Not in use right now" line of each card at once (26 px), and a fixed check on the Overview took its row away at once (75 px): every card below jumped.** `Show` adds and removes in one frame.
**Fix:** `Grow` (`common.rs`) eases the box (height, padding, margin, border) and the opacity from the place it has now to its resting place, read from the element, so the last frame is the resting frame; it closes before it removes. `eased` keeps a row that leaves at its place until it has closed. Used for the off line and the conditional fields of every settings card (`form.rs`) and the Overview check rows.
**Check:** the jump detector (the layout-shift observer plus a frame-by-frame watch of every running animation) found these; `tests/e2e/dashboard.js` checks that a route tab click keeps its tab row.
- **A wizard preset that is not on a card changes settings the owner never sees.** The first presets were two hidden switch names in `Card::preset`. *Avoid:* the wizard fills the values of `profile.json` for the section's paths that have no saved value, and a test fails for any value that is on no card (`wizard::tests::every_recommended_value_is_on_a_card`). A card that holds a number or a choice shows it with the schema's own field, so it saves with the same range check.
