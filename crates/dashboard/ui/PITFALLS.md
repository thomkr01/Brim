# Dashboard UI pitfalls

Pitfalls for the rest of the bot are in [PITFALLS.md](../../../PITFALLS.md) at the repo root.

These are Leptos mistakes that broke the dashboard in November 2026. They compile without a warning, so read this before you change the UI. The e2e sweep (`tests/e2e/dashboard.js`) checks for each of them.

## 1. A `>` in an attribute ends the tag

The `view!` macro reads an attribute value without braces only up to the next `>`. So this:

```rust
<button disabled=move || lo >= num on:click=move |_| step(-1.0)>"-"</button>
```

becomes `<button disabled=move || lo>` plus the text `= num on:click=move |_| step(-1.0)>-`. It still compiles. The page then shows that code as text, and the button has no click handler. Every number field with a minimum had this bug: the `-` button showed code and did nothing.

A `<` does no harm, because the macro does not treat it as the end of the tag. That is why the `+` button with `hi <= num` worked.

**Rule:** if an attribute value holds `>` or `>=`, wrap it in braces: `disabled=move || { lo >= num }`. You can also flip the comparison: `num <= lo`. Expressions inside `(...)` are safe, for example `.is_some_and(|a| a.len >= 25)`.

**Check:** the sweep fails if a page shows `move |` or `on:click=` as text. CSS can show the text in capitals, so the check ignores case.

## 2. Reading a signal after its page is gone

A page's signals and `StoredValue`s are disposed when you leave the page. Most reads of a disposed value panic:

```
panicked at reactive_graph-0.2.16/src/traits.rs:361:29:
Tried to access a reactive value that has already been disposed.
```

Rust in wasm cannot recover from a panic, so the whole dashboard stops ("Dashboard failed to start: RuntimeError: unreachable executed"). Reload the page to get it back.

This happens when code runs **after the page is gone**:

- the rest of a `spawn_local` task after an `.await`, while the request was still running when you changed page;
- a `set_timeout`, `request_animation_frame` or `later(...)` callback;
- a `load` closure called again after an await, for example `Ok(_) => load`, because `load` reads `g.get_value` or `page.get_untracked`.

A slow or failing server makes this much more likely. That is the normal case while the dashboard restarts during an Update, and Update is where the crash first showed.

What panics and what does not:

| Call on a disposed value | Result |
|---|---|
| `.get`, `.get_untracked`, `.with(..)`, `.with_untracked(..)`, `.read` | **panics** |
| `StoredValue::get_value`, `with_value(..)` | **panics** |
| `NodeRef::get_untracked` | **panics** |
| `.set(..)`, `.update(..)`, `update_value(..)` | does nothing, safe |
| `.try_get_untracked`, `.try_with(..)`, `.is_disposed` | returns `None` / `true`, safe |

**Rules:**

1. After every `.await` in a `spawn_local`, check that the page still exists before you read anything:
   ```rust
   let r = api("GET", &path, None).await;
   if data.is_disposed {
       return;
   }
   ```
2. A closure that can run again after an await (`load`, `load_mine`, ...) starts with `if g.is_disposed { return; }`.
3. In timers and frame callbacks, read with `try_` (`msg.stay.try_get_untracked == Some(false)`, `pv.try_get_untracked.flatten`).
4. `run_act`, `run_change` and `run_save` (in `common.rs`) skip their callback if the page that started them is gone. Use them for actions instead of writing your own `spawn_local`.
5. Setting a value is fine without a check. Only reads panic.

We did not switch every task to `spawn_local_scoped_with_cancellation`. It also cancels a task when only a smaller part of the page is removed, for example a dialog that closes itself. The save's follow-up step would then never run.

**Check:** the sweep switches through every page while each API response is held back 800 ms, then again while every request fails. Any panic fails the test.

## 3. A hover rule beats the "selected" rule

`nav a:not(.srv):hover` has a higher specificity than `nav a.on`, because `:not(.srv)` counts as a class. Under the mouse the selected link (the server on the server list, the open page in the menu) got the dark hover background while its text stayed dark: 1.1:1 contrast, almost invisible.

**Rule:** a rule for a selected or active state lists its `:hover` variant with the same selectors as the hover rule: `nav a.on, nav a.on:not(.srv):hover { ... }`.

**Check:** the `contrast` phase of `tests/e2e/deep.js` hovers every link and button on every page in light and dark mode and fails under 3:1.

## 4. A page that reads a context signal is rebuilt when the signal is set again

The page closure in `App` read `access.of(p)`. The access is fetched again on every page change, and setting a `RwSignal` notifies even when the value is the same, so every page was built twice and loaded its settings, schema and Discord data twice. The page was also built before the access answer came, so hidden pages started their requests anyway.

**Rule:** read a value that is fetched again through a `Memo` (`page_access`), which only notifies when the value changes. Decide what to show before you build the page's components.

## 5. A dropped browser promise hides a refusal

`clipboard.write_text` returns a promise. `let _ =` dropped it, the button said "Copied!" when the browser refused, and the refusal surfaced as an uncaught page error. That included the Rescue Key, which is shown once.

**Rule:** await browser promises with `JsFuture` and show the result. Use `copy_text` and `copy_label` in `common.rs` for copy buttons.

## 6. An emptied number field saved `null`

Clearing a number field stored `null`, and almost every number setting is a plain `i64`/`u32`/`f64` in Rust, so the save was refused with "invalid type: null, expected i64". A float step also kept the float error (`0.01 + 1 - 1` gave `0.010000000000000009`).

**Rule:** `Field` sets the field's default when a number input is emptied, and float steps are rounded like percent steps. A new number field gets a default (`dv`) unless `null` means something.

## 7. A descendant selector styles more than the field title

`.row label { flex:0 0 100%; font-weight:700 }` is meant for a field's title, but it also hit the Import button of every list field (a `<label>` around the file input). Import grew to the full row, turned bold, and the help text was drawn on top of it (Join Gate's Strict and Wildcard Words).

The first fix (`.io label`) did nothing: it had the same specificity as `.row label`, which comes later in the file, so `.row label` still won. The same rule also made every checkbox in role and channel lists bold and capitalized.

**Rule:** a label that wraps a control (`.row label:has(> input)`) resets what `.row label` sets. When a rule must beat another one, give it a higher specificity, not just an earlier or later spot in the file, and check the result in the browser (`DEEP_ONLY=overflow` measures it). A list field's box takes the full row so its help text goes below it.

## 8. Layout breaks that only show at some widths or with long text

Help text next to a switch was squeezed into two thin columns (Join Gate "Exclude bots added by the owner", Miscellaneous, Auto Mod, Verification), and at 390 px the header was wider than the screen, so the avatar was cut off and the search box covered the logo.

**Rule:** a switch's help text sits on its own line under the label (`.tog > .muted`). On a phone, header links that do not fit move into the account menu (`.m-only`). `DEEP_ONLY=overflow node tests/e2e/deep.js` checks every page at 1400, 1024 and 390 px for text outside its box, cut off, on top of other text, or squeezed.

## 9. One version per widget on a page that saves several sections

Each Overview switch loaded the config and kept its own version, so after one switch saved, the next one sent an old version and was refused with "config changed, reload". Five switches also meant five identical config requests.

**Rule:** widgets on one page that save into the same config share one loaded config and one version signal, and update it after each save.

## 10. Many copies of one SVG icon share its ids

Every icon in `assets/icons` has fixed ids (`info-grad`, `info-cut`). With the info icon shown many times on a page, every copy paints with the first copy's gradient and mask, and draws nothing once that first copy is hidden (Chrome does not render a gradient from a `display:none` SVG).

**Rule:** `icons::icon` numbers the ids of every copy, so every icon is safe to repeat. Never inline an `assets/icons` SVG another way (for example `include_str!` into `inner_html`) without the same numbering.

The first fix added a separate `icon_unique` for the info icon only, while the arrows in sortable lists, the check rows on Overview and the sidebar icons (General, Filters and Panic Mode appear under both Auto Mod and Anti Nuke) still shared ids. Root cause: uniqueness was opt-in. It is now the only way `icon` works.

## 11. A transform on a parent moves its popup too

The info icon grew on hover with `transform:scale` on `.tip`, which also scaled the card inside it, so the card was wider than measured and crossed the screen edge on a phone.

**Rule:** animate the icon (`.tip .i`), not the element that holds the popup. The info card is placed when it opens (`Tip` in `common.rs`): shifted to stay inside `main` and the screen, and moved below the icon near the header. `DEEP_ONLY=overflow` opens every info card and checks it.

## 12. `AnimatedShow` can hide itself while `when` is true

Leptos's `AnimatedShow` starts a hide timer each time it reruns with `when` false, but only remembers the last timer. A form that loads sets `val` and `orig` one after the other, so `when` (`val != orig`) reruns false twice. The first timer is lost and fires later. If the user changes something within 200 ms, the "Changes detected!" bar shows and is hidden again at once. Nothing can be saved or cancelled until the next change. The Cancel e2e check hit this on a fresh server (#158).

**Rule:** use `common::Fade` instead of `AnimatedShow`. It puts `when` behind a `Memo`, so it reruns only when the value really changes. All 8 uses were switched.

Same class of bug, also in #158: the page wrapper read the access level straight from the access signal, so every access answer (one per page change) rebuilt the open page and could throw away edits made before it arrived. It now reads a `Memo` of the level.

**Rule:** when a reactive closure builds a whole page or form, read derived values through a `Memo`, not a signal that is set again with the same content.

## 13. A hidden popup still makes the page wider

The info card (`.tip .bubble`) is only `opacity:0` until hovered, so it still takes layout room. An info icon near the right edge on a phone (MCP tokens page, 390 px) put the hidden card past the screen, and the whole page scrolled sideways.

**Rule:** `main` has `overflow-x:clip` (clip, not hidden, so vertical overflow and sticky parts keep working). A new popup that is hidden with opacity or a transform must also stay inside `main`, as `Tip`'s `place` does when it is shown.

**Check:** on a 390 px viewport `document.documentElement.scrollWidth` equals the viewport width on every page.

## 14. An effect on a new input runs before its `prop:value` is set

Channel Setup's rename box focused and selected its text from an `Effect` on the input's `NodeRef`. The effect ran before `prop:value` was applied, so `select` selected an empty box, the name was filled in afterwards, and typing added to the old name instead of replacing it. A `select` in `on:focus` had the same problem.

**Rule:** an effect that needs a new input's value sets it first (`i.set_value(..)`, then `focus` and `select`). No other page selected text on open.

## 15. Overlapping saves stored an older draft

Channel Setup sent a PUT for every edit. Two in flight could reach the server in either order, so the older draft could be the one stored, and the refused-save path set the page from a follow-up GET without checking that it was still the newest request. An edit made before the first GET answered built on an empty draft and replaced the stored one.

**Rule:** a page that saves on every edit keeps one request in flight: later edits change only the local copy and mark it dirty, and the reply sends the newest copy if it differs. Every reply that sets page data after an await checks its sequence number with `try_get_value` (rule 2). Actions that need the saved state (Push, Discard) are disabled while a request is pending, and editing waits until the first load is in.

## 16. A panel rebuilt from every reply loses the focus

Channel Setup's right panel is rebuilt from the server's reply after each change, so the button, switch or picker that was just used was replaced and keyboard focus fell back to the page.

**Rule:** a reactive closure that rebuilds controls notes the focused control's `aria-label` before it builds and focuses the new control with that label in a `request_animation_frame` (`panel_focus`/`refocus` in `channels.rs`). Every control in such a panel needs a unique `aria-label`.

## 17. A select that acts on `change` fires on every arrow key

In Chrome on Windows and Linux, pressing an arrow key on a closed `<select>` changes its value and fires `change`. Channel Setup's "+ Role" picker added the role at once, so a keyboard user who arrowed through the list added every role on the way, and the first version of the new add-channel menu would have created a channel per key press.

**Rule:** a select that creates or adds something only picks; a button next to it does the action (`kind to add` + "+ Add", `role to add` + "+ Role" in `channels.rs`). A select that only sets a value the user saves later (the settings forms) can keep `on:change`.

## 18. `document.activeElement` does not say which button was clicked

The shared 2FA prompt first picked the control to click again after a good code from `document.activeElement`. Safari (macOS and iOS) does not focus a button on click, so the saved element was the page body or a control focused earlier, and the retry could press a different button with its confirm dialog skipped. Review caught it before merge.

**Rule:** to know which control started an action, use `common::clicked` (a capture-phase click listener records it and clears it after the click). Read it synchronously in the handler; after an `.await` it is gone, so capture it first and pass it on (`need_step_up(need, trigger)`).

## 19. A number field's +/- did not fit its range

Two bugs in `Field` (`form.rs`), found by the deep test (11-15). Anti-nuke Panic's "Heat degradation" was a `float` field: + and - step by 1, but its range is 0.001-0.99, so + went to 0.99 and - to 0.001, never back to 0.01. Join Raid's "Minimum matches" (`id_flag_min`, no default, empty = off, range 2-500): + from empty gave 2, and - was then disabled at the bottom of the range, so the field could not be turned off again with -. The same was true for Join Gate's optional age override (duration field, range from 5 s).

**Rule:** a fraction with a range below 1 is a `pct` field (it steps by 1 %), like Auto Mod's heat decay, not a `float`. A number field without a default keeps - enabled at the bottom of its range, and - there empties it (`at_floor` in `Field`, both the number and the duration branch). Searched: `similarity_floor` (0-1) had the same problem and is a `pct` field now too; the other `float` fields (`heat.max`, heat weights, `webhook_multiplier`) step by 1 without trouble. A range that depends on another setting (heat weights up to `heat.max`) comes as `max_path` in the schema, and `Field` reads it from the section, so the form and the server agree.

## 20. A long string without spaces in a table cell widened the page

The Raider List evidence table shows the Detail JSON (`{"case_kind":"ban","case_no":1}`) in a `<code>` cell. It has no break points, so on a 390 px phone the table pushed past the card and the screen (deep test overflow phase, 11-15).

**Rule:** `td code` wraps anywhere (`style.css`). A new cell with an id, JSON or URL uses `<code>` or its own `overflow-wrap:anywhere`. Searched: the overflow phase over every page at 1400, 1024 and 390 px found only this one.

## 21. The rescue key QR code was never blurred

The Rescue Key page gave the QR code `class:blur`, but the CSS rule was `pre.blur`, so only the key text was blurred and the QR was readable as soon as it showed. The key and QR also shared one Show button, and a new key kept the earlier Show state.
*Avoid:* a secret QR gets its own `.qr.blur` rule (a stronger blur, so it cannot be scanned) and its own Show button. When a class is added to a new element, check that the CSS selector is not bound to one tag. Reset every reveal when a new secret is made.

## 22. "Too many requests, slow down" hid a 2FA lockout

Every 429 from the dashboard said "too many requests, slow down": the per-session request limit, the 2FA lockout after 5 wrong codes, and the bot profile's 30 changes an hour. After a 2FA reset the owner saw only that text and could not know he was locked out, or for how long.
**Rule (owner decision 11-15): state the issue, not that "there is a problem".** An error says which limit or check was hit and what the user can do (wait how long, what to change). `ApiError::TooMany` takes its message, so every new limit must write its own. Say no more than the user needs: the lockout and its length, never what an attacker could use (which code was close, the counter).

## 23. A page loaded right after another showed nothing (request limit)

The e2e Logging check failed now and then: the page's `/config` and `/schema` came back 429. The per-session limit allowed 20 requests at once, and the Tickets tab alone sends about 12 on load, so a quick switch to the next page ran out. A real user who clicks fast sees an empty page.
*Avoid:* `auth::BURST` (40) must hold two page loads in a row. A page that adds many requests on load needs a check against it; the steady rate (20 a second) stays the guard.

## Where it was fixed (#143)

- `form.rs`: the `-` button on number fields (`>=` in the attribute); the settings card's config/schema load; the preview flash frame callback; list import.
- `update.rs`: the update status request.
- `pages.rs`: the setup score request.
- `common.rs`: the toast timer, and the shared `run` behind `run_act`/`run_change`/`run_save`.
- `appeals.rs`, `embeds.rs`, `lockdown.rs`, `records.rs`: the `load` closures that run again after an action.
- `main.rs`: config import after the file is read.

**An emptied text field saved null, and the save was refused.** (Tickets 2.0)
Greeting, closing, pause reason and ban appeal message are plain strings in the config, but the form sent `null` when the box was cleared, so the save failed with "invalid type: null".
*Avoid:* an emptied number or text field saves its schema default. Give every text field that maps to a plain `String` a `dv(.., "")` default in schema.rs.

## 24. The account menu stayed open after a choice

The account menu and the bell are `<details>` in the header. A link in them changes only the hash, so the page changed and the menu stayed open on top of it, and it moved with its button when the header changed (the Update page has no search box). A tap outside did not close it either.
*Avoid:* `close_menus` (main.rs) closes every open header `<details>` and every row ⋯ menu (`details.rowmenu`) on any click outside its own button, on a hash change and on Escape. A new dropdown is a `<details>` inside `header` or with the class `rowmenu`, so it gets this for free; a dropdown made another way must close on the same three events (the message builder's popups have a `.popbg` backdrop). The Feeds ⋯ menu stayed open on a click outside (2026-11-21): the rule above covered only the header.

## 25. A system toggle jumped left while its toast showed

`Fade` wraps its child in `AnimatedShow`'s `<div>`. The toast inside it is `position:fixed`, but the empty wrapper div is not, so in the Overview card footer (`.sys .foot`, flex with `space-between`) it became a third flex item and pushed the toggle to the middle until the toast closed.
*Avoid:* `Fade` gives its wrapper the class `fade-wrap` (`display:contents`), so the wrapper makes no box anywhere it is used. A component that adds a wrapper element must not change the layout of the place it is dropped into; check it inside a flex row.

## 26. The Overview score did not follow a system toggle

A system toggle on the Overview saved the config but did not load the checks again, so "Join gate is off" stayed listed after Join Gate was turned on, until a page reload.
*Avoid:* `SystemToggle` takes a `saved` callback, and the Overview passes one that reads the score again. Not the config: that read could land after a second switch's save and show the second switch in its old state. Another page that changes a setting needs nothing: the Overview loads both each time it opens.

## 27. A text box typed in place lost the cursor on every key

The Message Builder types on the message itself (`contenteditable`). A view that reads the whole message rebuilds every box on each key, and setting a box's HTML while it has the focus puts the cursor back at the start.

**Rule:** the editor is rebuilt only from a `Memo` of the message's shape (embeds, fields, inline flags, buttons per row), never from the text. Each box (`Ed` in `embeds.rs`) writes its HTML only while it does not have the focus and only when the text differs. Paste and Enter are handled by the box, so no other page's HTML gets in.

## 28. A popup that grows after it opened ran off the screen

The footer-time popup opens small and grows when "Date and time" adds the calendar, so the check that moves it up (done once, on open) no longer held and the time box was cut off.

**Rule:** a popup that moves itself to stay on screen checks again whenever its content can change (the Message Builder popup re-checks on every change to the message).

## 29. Typed JSON put text where the editor expected an object

The JSON view lets a user write `{"embeds": [1]}` or `"fields": ["x"]`. The editor then indexed that value by key (`e["fields"]`, `em["fields"][i]["inline"]`), and serde_json panics when a string or number is indexed mutably by key or an array past its end.

**Rule:** every write into the message goes through `pointer_mut`/`get_mut`/`as_object_mut` and skips a value of the wrong shape (`put`, `edit_embed`); a popup for an embed or button row closes when that embed or row is removed. The server refuses fields that are not objects.

## 30. A chip class name took a popup's layout rule

The chip for a member variable had the class `mo`. Inside a popup it matched `.pop .mo` (the calendar's month row: `display:flex`), so the chip in a menu answer filled the whole line.

**Rule:** short class names in popups are taken (`.pop .mo`, `.pop .row`, `.pop .seg`). Give new inline parts a name of their own (`vchip`, `mem`) and check them inside a popup too.

## 31. Escape did nothing while the text box kept the focus

The toolbar buttons keep the cursor in the text box (`mousedown` + `prevent_default`), so a popup opened from them never had the focus and its own `keydown` never saw Escape.

**Rule:** Escape for the builder's popups is one `window_event_listener` (removed `on_cleanup`) that closes the top popup first.

## 32. `move || a || b` as a `view!` attribute did not compile as one closure

`disabled=move || busy.get || n >= max.get` was read by the `view!` macro with the second `||` outside the closure, so the types did not match; with parentheses it compiled but clippy refused the extra parentheses. (Feeds page, 11-20.)

**Rule:** an attribute value with `||` or `&&` in a closure is a named closure first (`let off = move || busy.get || n >= max.get;` then `disabled=off`).

## Enter that confirmed an IME word also submitted the box (modern web pass, 2026-11-19)

Typing Chinese, Japanese or Korean, Enter picks the candidate word and fires a `keydown` with `key == "Enter"`, `is_composing` true (keyCode 229 in older browsers). Every `if e.key == "Enter"` handler submitted or inserted a line break in the middle of a word.

**Rule:** a handler that acts on Enter calls `common::is_enter(&e)`, never a plain `e.key == "Enter"` check.

## A typed marker put link HTML into an attribute (audit, 11-17)

`md::inline` stashes code spans, links and `<...>` tokens as `U+0001 n U+0001` and swaps them back in order on the whole string. `esc` leaves U+0001 alone, so an emoji named with a typed marker (`<:\u{1}1\u{1}:42>`) had the next link's HTML swapped into its `alt`, and the link's `"` closed the attribute: `onerror=` ran when an Owner opened the preview (stored XSS from Editor level).

**Rule:** text that a stash-and-swap pass works on must not be able to hold the marker: `inline` drops U+0001 before it stashes anything, and the server refuses control characters in saved messages. Searched: no other UI pass swaps placeholders back into HTML (`wizard::fill` fills JSON-escaped values into sample JSON, `icons::fill_tags` puts fixed svg only).

## A failed save hid the Save bar (audit, 11-17)

The settings form and Permits set `orig` to the edited values before the request. When the save was refused (a conflict, a bad value, or 2FA needed first) the "Changes detected!" bar disappeared, the fields kept the edit, and a later save of another section never sent it. After the 2FA code there was no button to press again.

**Rule:** the saved state (`orig`) follows the server: set it only from a good answer, per section when several are saved one after another (embeds.rs already did this). A `saving` signal disables Save while a request runs, so a double click does not send the same version twice.

## Older answers overwrote newer ones (audit, 11-17)

Record lists (`records::fetch`), appeals, the members search, the trigger "Try a message" box (a POST per key), the schedule "Next:" preview, and the sticky, trigger and schedule lists of a message wrote whatever answer came last. A quick filter change, or opening message B right after A, showed the old rows under the new filter or message.

**Rule:** every request whose answer sets page data checks after the await that it is still the newest: a sequence number (`StoredValue`, or `fetch`'s per-list counter) or the id it was asked for (`embed`). A request per key waits 300 ms after the last key (as `CaptchaPreview`).

## Typed text was cleared before the server took it (audit, 11-17)

"Add note" and Lockdown "Send an update" cleared the box in the click handler, so a refusal lost the text. The appeal reply box was never cleared, so a reply for one member went out with the next decision on another.

**Rule:** clear a text box in the success branch of its request, and clear a box that belongs to one item after that item's action worked.

## Message Builder: Cancel and Save did less than they showed (audit, 11-17)

Cancel restored name, body and channel but not Pings and "Send as", although `snapshot` and the dirty check hold all five, so a picked "@everyone" stayed and was saved later. In the JSON view a text that did not parse kept the last valid body, and Save said "Saved." and leaving gave no prompt. A plain Save did not set `busy`, so a double click on a new message inserted it twice.

**Rule:** Cancel restores every part `snapshot` holds. Save and leaving check the JSON view's error first. Every save sets `busy`, and the bar's Save is disabled while it runs.

## A page built on a config it did not have (audit, 11-17)

After an action, the settings form set the new version before it read the new config, so a failed read left old values with the new version and the next save passed the conflict check and overwrote what the action did. A failed first load showed the defaults as if stored. Overview turned a failed `/config` into `{"config": {}}` and showed every system off.

**Rule:** a version is set together with the config it belongs to. A page whose config did not load says so and does not offer Save or switches (they stay locked) until a reload.

## Errors that hid the cause (audit, 11-17)

`reply` showed "request failed" for every answer without our JSON error (a 413 for a large import, a proxy 502 during an update) and the raw `JsError` for a network error. A failed emoji list showed "This server has no emojis of its own." and was cached; a failed `/schema` left search with page names only, silently; the raw JSON editor dropped the parser's line and column; a number box ignored text that was not a number.

**Rule:** an error names the actual issue: the status and text when the body is not ours, "Could not reach the dashboard server" for a send error, the parser's message. A failed list is shown as failed and asked again the next time, never as empty.

## A Rename approved an unreviewed AutoMod rule (audit, 11-17)

The pencil sent the whole rule through `PUT`, and the server approves the rule on every save, so one click approved a change made in Discord without review. Bulk Activate/Deactivate fired all requests at once, each with its own reload, and a refused switch (changed in Discord already) reloaded nothing.

**Rule:** a paused rule is renamed only through "Review and approve". Bulk actions run one after another, then reload once whatever the answers were, and show every error.

## A comma list split an entry that held a comma (audit, 11-17)

`list` fields show `csv` and save `from_csv`, but Import adds one entry per line, so `hello, world` became two words on the next edit (a phrase filter turned into two single-word filters).

**Rule:** a list that holds an entry with a comma shows one entry per line (the `lines` box). A comma typed into the one-line box still splits the entry.

## A one-click setup dropped unsaved edits (audit, 11-17)

Logging, Catch and Tickets setups rebuild the form from the server after they ran, so edits typed before the click vanished; the setup also read a fresh version, so the conflict check could not protect them.

**Rule:** `Section` reports its dirty state (`dirty` prop); an action that rebuilds the form is disabled with "Save your changes first" while it is set.

## The message preview loaded any image address (audit, 11-17)

The Discord-style preview used thumbnail, author, footer and image URLs as they were, without the `https` check the editor has, so a saved embed could make a viewer's browser call any host.

**Rule:** a preview loads only `https://` images; other addresses are left out.

## The dashboard font came from Google (audit, 11-17)

`index.html` loaded Poppins from fonts.googleapis.com, so every visitor's IP address went to Google, while PRIVACY.md said nothing goes to third parties.

**Rule:** the UI loads nothing from another host: fonts, scripts and styles are files under `assets/` that Trunk copies into `dist` (`assets/fonts`, with its licence). The only exception is Cloudflare Turnstile on the verification page, which PRIVACY.md names.

## A bare `.chip` rule restyled every chip (audit, 11-17)

The Message Builder block in `style.css` declared `.chip { background:none; ... }` for its weekday chips, after the base `.chip` rule, so every plain chip in the app (OWNER, APP, the lockdown chips) lost its filled look. `@keyframes pulse` was also defined twice; the later one won everywhere.

**Rule:** a page's rule is scoped to its container (`.chipset .chip`), and an animation name is defined once (search `@keyframes` before adding one).

## A warning closed before it could be read (Message Builder, owner decision 2026-11-17)

Every toast, errors included, closed after a fixed 3 seconds (`TOAST_LIFE`), whatever its length. `Msg` had a `stay` flag for toasts that wait for a click, but nothing set it any more, so the earlier rule "a refused save stays up" (root PITFALLS, dashboard) was silently broken on every page. Owner decision saw "Sent, but Discord could not load ..." vanish before he could read it.

**Rule:** `msg.err` (errors and warnings) stays until clicked or replaced; an ok toast stays 3 s plus 50 ms per character past 60, and its strip runs as long. A warning about one item is also shown next to that item (the Sent log row keeps a copy's picture warning). The toast sits at the top, so a stuck one never covers the Save bar at the bottom on a phone.

## The account button moved between pages (owner decision 2026-11-16)

Long pages (a server's settings) get a scrollbar and short ones (the server list, Commands) do not. Where the system shows classic scrollbars (a Mac with a mouse, Windows), the scrollbar takes about 15 px from the page width, so the header's right side (bell, theme, account) jumped left on long pages. Headless test browsers hide scrollbars, so the tests never saw it.

**Rule:** `html { scrollbar-gutter: stable; }` keeps the room for the scrollbar on every page. To check a layout jump, launch the test browser without `--hide-scrollbars`.

## "Update now" showed when Brim was up to date (owner decision 2026-11-16)

The Update page offered the update whenever Running and Newest were different commits. After an update, Running can be newer than Newest (Newest is cached for a minute, and the images of a newer commit can be out before the list shows it), so each press restarted the bot and the dashboard for nothing. Root cause: "not the same" was read as "newer".

**Rule:** the dashboard asks GitHub's compare which one is ahead (`update::newer`) and the page offers the update only when Newest is ahead; only when GitHub cannot answer does it fall back to "not the same". Searched: the header's Update link has no "new version" mark, so nothing else compares the two.

## A double click posted twice (Message Builder 3, 2026-11-16)

Send and Edit message started a request on every click, so a quick double click posted two copies. A row without an id would also have turned "Edit message" into a Send, since 0 meant "new copy".

**Rule:** a button that posts to Discord waits while its request runs (a `busy` signal), and a missing id disables the action instead of falling back to a value with another meaning.

## An icon tag broke a link in the preview (security review, 2026-11-17)

`md` swaps `{e:name}` icon tags for icon HTML after the rest of the HTML is built, so a tag typed inside a masked link's target (`[x](https://a/{e:case})`) put the icon's quotes into `href` and closed it early. The leftover attributes were fixed icon markup, so no script ran, but the link broke.

**Rule:** a pass that runs on built HTML must not reach into attributes; encode what it looks for (`{` and `}` as `%7B` / `%7D`) in every attribute value first. `xss_tests.rs` fuzzes the preview renderers for this.

## A server error toast said only "internal error" (owner decision, 2026-11-18)

The toast had no way to find the cause in the log. Now the server sends "Server error, code XXXXXXXX. The cause is in the dashboard log." (or a recognised cause's plain text plus " (code XXXXXXXX)") and logs the cause under `error_id=XXXXXXXX` (root PITFALLS, section 6).

**Rule:** the UI shows the server's error text as it is; never replace it with a fixed text, or the code is lost. `run_act` lets a refused action's toast close by itself, which hid the code after about 3 s; `run` now keeps any toast that starts with "Server error, code " or ends with " (code XXXXXXXX)" up until clicked. If the server text changes, change that check too.

## A new class name picked up another widget's style (score ring, 2026-11-18)

The score ring's landing animation used the class `pop` on its number. `.pop` is already the popup box of the Message Builder (border, background, shadow), so the number showed inside a dark square while it animated. Only a render showed it; clippy and tests passed.

**Rule:** before adding a class, grep `style.css` for `\.<name>\b`. Pick a name that is unique in the file, or scope the rule under its component (`.ring .num.landed`), and render the change once.

## Loading looked different on every page (owner decision, 2026-11-18)

Pages showed "Loading...", a button box with dots, or nothing while a slow action ran. Owner decision: the dots and fun word indicator is the one loading indicator for the dashboard.

**Rule:** every loading or working state uses `<Working />` from `common.rs`: four dots and a fun word, no box (a button that shows it drops its box), the word changes every 4 s, the dots glide slowly. Do not add a spinner, a "Loading..." text or another indicator. Under reduced motion the dots pulse in place, so the global reduced-motion rule must not stop them.
- **The published dashboard wasm was 37.7 MB because trunk skipped wasm-opt.** (owner decision, 2026-11-18) `index.html` had no `<link data-trunk rel="rust" ...>`, so trunk builds a default app, and that default has wasm-opt off (`new_default` in trunk's `pipelines/rust`); the wasm and its 3.1 MB gzip were served for every page load. A local `trunk build --release` of the unchanged files gave 38.1 MB. **Rule:** the `rust` link stays in `index.html` with `data-wasm-opt`, and `[profile.release]` in `ui/Cargo.toml` stays on `opt-level = "z"`, `lto = "fat"`, `codegen-units = 1`. The built wasm is 3.2 MB (1.2 MB gzip, 0.87 MB brotli); check the size of `dist/*_bg.wasm` after a build, a build that is 30 MB again lost the link. Measured in a browser against the old build: page switches and the full e2e sweep took the same time, the first load was faster.


## A page drew its "not available" state before the first answer (Update page, owner decision 2026-11-20)

The Update page started with `data = Null` and drew at once: "Running: unknown", "Could not reach GitHub right now." and "The updater is not running", for the moment until `/api/update` answered. The Commands page did the same with "The bot has not published its commands yet". Cause: every field of a `Null` value reads as missing, and a missing field was taken as "no".

**Rule:** a page that loads one `Value` shows `<Working />` while it is `Null` and draws no empty, "unknown" or "not running" state until the first answer is in. A first answer that fails shows an error line, not the loading sign for ever. Checked: the other pages that start with `Value::Null` hide their card, show a skeleton or `<Working />` until loaded.

## "Diff vs current" did nothing you could see (History, owner decision 2026-11-20)

The button wrote its answer into a `<pre>` at the bottom of the page, below every version row (200 of them), the layout versions and the AI reads. The request worked, but the answer was off screen, with no loading sign, and a failed history load was dropped without a message.

**Rule:** the answer to a button goes where the person is looking: a dialog (as the settings import preview) with `<Working />` while it loads and an error line that states the cause if it fails. Never write a result into a block far from the button. Every load that can fail shows its error (`Err` is not dropped). History rows show the member's name (`records::learn_names` + `records::who`) with the ID beside it and the time in the viewer's local time. Checked: the Revert Center result sits right under its button; no other button writes to a far block.

## A button inside Channel Setup was invisible (Layout Guard L1, 2026-11-18)

The "Fix on Overview" button in the Layout Guard card did not show: `.cs .acts` sets `opacity:0` for the tree's row buttons (they show on hover or select), and the card sits inside `.cs`, so every `.acts` in it was hidden too.

**Rule:** a new card on a page with a wrapper class (`.cs`, `.wizbody` ...) checks the wrapper's rules for the class names it uses, and scopes its own override (`.cs .lg .acts { opacity:1; }`). Look at a screenshot of every state, not only the default one.

## The Overview page read a signal after its page was gone (e2e flake, 2026-11-18)

The "page switch" e2e failed once with `Tried to access a reactive value that has already been disposed`. The cause was the Overview (score) page's `load`: after the config request it matched `Err(e) if all.get_untracked.is_none`, which reads `all` without the check from pitfall 2. When the request failed after the user had left the page, the read panicked. The panic exists with any build; the timing of a failed request decides if the sweep hits it.

**Rule:** the check is `if x.is_disposed { return; }` right after the `.await`, before any `match` arm or guard that reads a signal. A `match` guard (`Err(e) if sig.get_untracked...`) is a read too. A search for `spawn_local` blocks that read after an `.await` without `is_disposed` or `try_` found no other page with this pattern.

## "Add panel" on an empty ticket panel list made a bare panel message (owner decision, 2026-11-18)

With no panel saved, "Add panel" under "More buttons and forms" pushed one "New panel" button and left the message text empty, so #open-a-ticket showed only the title "Open a ticket" and that button. Only "Edit the ticket button" filled the default text. **Rule:** every control that makes the first item of something the bot posts brings the defaults that go with it, the same as the main start button (`form.rs` panels: `start` and `add`). Searched the other "Add" buttons on the form (form questions, snippets, ticket types): their lists are posted without a text of their own.

## A design token name was already a dashboard variable (design system, 2026-11-18)

The design system's tokens are global custom properties (`--page`, `--line`, `--ok`, `--bad`, `--warn`, `--info`). `style.css` already had `--page` (the old gradient) and the four status names. Loaded together, the later file silently won, and a type class from the design tool's generated tokens.css (`.small`) would have restyled every `.small` in the dashboard.

**Rule:** colours, spacing, corners and font stacks come only from `design/tokens.css`, generated by `tools/design-tokens.mjs` (custom properties only, no `@font-face`, no type classes). `style.css` never defines a variable a token already names; its short names (`--b1`, `--fg`, `--primary` ...) only point at tokens. Before adding a token, grep `style.css` for `--<name>\b`. CI checks that `tokens.css` matches `tokens.json`.

## Animations always play; hover never lifts lines or scene pieces (front page, 2026-11-23)

Animations are always on for the whole site (owner decision): there is no motion switch, no saved choice and no rule that stops them, also when the system asks for less motion. `design/bundle.css` no longer cuts `tb-` animations under `prefers-reduced-motion`; it only removes the 2 px hover lift.

The front page lifted on hover because the global rule for buttons (`style.css`, `button:hover`) and for `.card.link` moved the line buttons and the scene pieces up. The shield outline looked thicker on one side because an inner shield cut with the same percent polygon is thinner on the slanted sides.

**Rule:** on the front page a line only turns grey to white on hover; nothing moves (`.land .feat-tab`, `.land .scene` pieces). A hover rule written for all buttons must be checked on the front page. A shape outline is a stroke (mask), never a smaller copy of the shape.

## Every page had its own sizes (owner decision, review, 2026-11-19)

`style.css` used 25 font sizes, 16 corner radii and 56 paddings, and 14 pages set sizes inline with `style="..."`, so titles, help text and cards looked a little different on every page.

**Rule:** a size comes from a token: text from `--fs-*` (the design system's type styles, generated into `design/tokens.css`, plus `--fs-figure` and `--fs-page-title-phone` at the top of `style.css`), padding from `--space-*`, corners from `--radius-*`. `0`, a percentage and `em` (relative to a token) are fine. A page never sets a fixed `style="..."`; it uses a class (`.grow`, `.fullrow`, `.vstack`, `.center` ...). A computed style (a colour, a position, a CSS variable) is fine. `tools/ui-sizes.mjs` fails CI otherwise. A new size goes into the design system first. `#/ui` shows every block and size in one place: check a change there.

**Five text sizes (universal layout PR 1, 2026-11-20):** the dashboard uses `page-title` (44, 32 on a phone), `figure` (26), `section-title` (18: headings, card titles, crumb, brand), `ui`/`ui-strong` (14: body, buttons) and `caption` (12: help lines, chips, table headers, code). The old 11, 13, 15, 16, 17 and 19 px tokens are gone from the Dashboard group; a rule that needs a size picks the nearest of the five. `tools/ui-sizes.mjs` fails on any other `var(--fs-*)` in `style.css`. The landing page types (`headline`, `lead`, `body`, `small`, ...) belong to the website part of the design system and are not touched.

**Page widths and rhythm (universal layout PR 2, 2026-11-20):** the page column is `--w-wide` (1200 px, set on `main`); see "One page width" below for the wide steps. There is no narrow column any more (owner, 2026-11-21). Between blocks (header, tabs, cards, stack items) the gap is 24 px, inside a card 20 px. `tools/ui-sizes.mjs` fails on a `max-width` of 800 px or more that is not `var(--w-wide)`. Dialogs, previews and fields smaller than that keep their own max-width.

**One header, one tab row, a crumb instead of "Go Back" (universal layout PR 3, 2026-11-20):** six pages wrote their own `<div class="head">` (no crumb, no "?", a literal "Brim" label) and three "Go Back" variants (a ghost button on Filters sub pages, `.card .back`, `.casepage > .back`) sat beside route tabs and state tabs that were built by hand in 11 places. Every page header is now `Head` (in `common.rs`), a detail page passes `up` so the crumb reads "Module › Parent › Page" (Filters sub pages, cases; the Permissions dialog shows its own `.crumbline`), and every tab row is `PageTabs` with `Tab::link` (route) or `Tab::state` (page state). Tabs are fixed by the owner: a quiet row, an orange underline (`border-bottom-color:var(--primary)`) on the open tab, no pill box, never wraps, scrolls sideways. **Check:** `tools/ui-sizes.mjs` fails on `class="head"` or `class="tabs"` outside `common.rs`, and when the `.tabs` rules lose the underline or get a fill; the e2e sweep checks the open tab's computed bottom border and background on every page with tabs. Channel Setup's `.seg` switch is a declared exception and is not a `.tabs` row.

**Cards move up, no holes; one add button (universal layout PR 4, 2026-11-20):** the settings grid was a CSS grid (two columns only from four cards), so a short card (Status) left an empty hole beside a tall one (Auto timeouts) until the next row started. CSS columns would close the holes but rebalance on every height change, so a card that grows when a switch shows more rows jumps to the other column under the cursor. `Cards` in `common.rs` now splits the card list once into two fixed columns (first k cards left, the rest right, balanced by rows per card via `split_at`); each column is a flex stack, one column below 900 px, same order as the list. About 30 add buttons had five looks (green "+", ghost "+ Add", dashed wide boxes, the `Empty` dashed button, grey dashed preview buttons); every one is now `AddButton` (green pill, circle plus, a text naming the thing). **Check:** `tools/ui-sizes.mjs` fails if `.cards` becomes a grid, on a button text starting with "+" or `label="+` outside `common.rs`, and on a `dashed` button class. e2e selectors use `button.add:has-text(...)` (the plus is its own span, so `text-is` no longer matches).

Text case: field titles come from setting names, so `.row label::first-letter` makes them sentence case (not `capitalize`, which gave Title Case). Only chips are upper case; type button and heading text in sentence case ("Confirm", not "CONFIRM").

## A disabled button looked like a live one (style page, 2026-11-19)

The base `button` rule had no `:disabled` look, so Save while nothing changed, "Fix issues" with nothing to fix and every button a permit locks looked pressable; only a few page rules (`.vitem`, `.chrow`, the wizard arrows) faded theirs. Found on the new `#/ui` style page.

**Rule:** `button:disabled` is at half opacity with a not-allowed cursor (the design system's `.tb-act:disabled`), except a button that shows `<Working />`. A page rule for a disabled button only adds to that.

## Tab pages had no selected sidebar link (2026-11-19)

Raid Log, Join Gate Log, Raider Matches and similar tab pages have their own page key, and the sidebar compared keys with a list of special cases (appeals, reports, lockdown), so on those tabs no sidebar link was selected.

**Rule:** `nav_key` (main.rs) maps a tab page to its sidebar page, and both the sidebar and the header breadcrumb use it. A new tab page adds its key there.

## A five-column table squeezed into strips on a phone (Anti Nuke rule coverage, 2026-11-19)

The coverage table scrolled sideways inside its card, but the columns still shrank first, so the notes were one or two words per line and "Exempt permits" sat past the screen edge.

**Rule:** every table is `table.data` (layout step 5, 2026-11-20); on a phone each row turns into a block, so every cell after the first that shows a value needs `data-label` with its column name, or it shows with no label. A cell with no label (buttons, a detail row) takes the full width. A grid of cells (Channel Setup's Advanced grid) is `table.data matrix` and stays a scrolling table. `tools/ui-sizes.mjs` fails on a `<table>` without class `data`. The overflow test now ignores text that is only scrolled out of a horizontal scroll strip (the wizard steps), which it reported as cut off.

## A "+" let you add past the server's limit (P2 counters, 2026-11-19)

"Add new action" (auto warn actions, at most 5) and "Add snippet" (at most 100) had no limit in the UI, so a sixth action was only refused at Save with a server error. Other lists copied the limit by hand (`100` messages, `7` schedules, `12` panels, `5` and `6` form fields), so a change on the server would leave the UI wrong.

**Rule:** a "+" list takes its limit from the server: `/api/limits` (api.rs `limits`, from the constants the saves check) through `common::limit`, `room` and `<Count>`, or a `cap` the schema sends. Never type the number into the UI. A new limited list adds its key to `limits` and shows `<Count>` next to its "+"; an empty list shows `<Empty>` with a dashed "+ New ...".

## The sidebar lists modules, not pages (2026-11-19)

The sidebar now has a few general links and one line per module; a module's parts are rows on its module page (`#/<server>/module/<slug>`). A page that is not in a NAV group has no row, and the e2e sweeps (`dashboard.js`, `deep.js`) find pages through the sidebar and the module rows, so it would never be tested.

**Rule:** a new page goes into a NAV group in `main.rs` (that is what the header search, the breadcrumb and the module page read) and gets a one-line `blurb` in `modules.rs`. A part with an on/off switch is added to `modules::flag` so the row shows "Set up" while it is off. Old links `#/<server>/<page>` keep working; never remove a route.

- **Reading `access` inside the route closure rebuilt the page on every access fetch** (Notes e2e 400, 2026-11-19). `module_of(.., |k| access.of(k) ..)` ran inside the `move ||` that builds the page, so it tracked `access.pages`; the access is fetched again on each page change, so the page was rebuilt a moment after it appeared and typed text was lost (a real person typing early loses it too). The `page_access` memo exists to prevent exactly that. *Avoid:* inside the route closure read access only through `page_access`; wrap any other access read in `untrack(..)`.

## A log type or event needs its category on the Customize list (2026-11-19)

The Logging "Customize" lists (Brim log types and Discord events) are folded categories built from each option's `group` (`kind_group` and `event_group` in `schema.rs`). An option without a group lands in a nameless category.

**Rule:** a new log type goes into `LOG_KINDS` and gets a category in `kind_group`; a new `LogEvent` gets one in `event_group` (the same function feeds the per-event cards).

## A row list that reads the saved settings closes every open row (Commands, P6b, 2026-11-19)

The Commands rows are `<details>`; a list closure that read the per-command settings would rebuild every row on each save and fold the one the user was reading.

**Rule:** the list closure reads only what changes the rows themselves (the command list, the filter, whether the settings loaded). A row reads its own setting in a small closure (`entry("prefix")`), so a save changes two chips and nothing else. A button or tick box inside a `<summary>` stops the click (`stop_propagation`, plus `prevent_default` on a button) or the row opens with it. The settings are for the server last opened: the page names that server in the dialog, and shows the controls only to its owners after the settings loaded.


## An error from a dialog's Save showed behind the dialog (Commands, P6b, 2026-11-19)

The toast had `z-index:41` and a `.modal` has 50, so a refused save from a dialog that stays open (the per-command settings) showed dimmed under the scrim and could not be clicked away.

**Rule:** the toast is the top layer (`z-index:60`); nothing a page opens goes above it. A dialog that sends a request while it stays open is checked with a refused save.

## The 2FA code must count for the server the page saves to (Commands, P6b, 2026-11-19)

The 2FA prompt verified for the server in the address. The Commands page has no server there, so the code counted for no server and the save was refused again, without end. Reading `last_guild` from the browser store instead is wrong too: another tab can change it, and the code then counts for a server the page does not save to.

**Rule:** a page without a server in its address that makes a 2FA-guarded save tells the prompt its server from the same source its save uses (`Access.pages.guild` for Commands).

The prompt also opened behind the settings dialog (both dialogs had the same z-index and the prompt comes first in the page), so the save looked dead. **Rule:** a save made from inside a dialog needs the prompt above it (`.modal.top`); check it in the browser with a login that has no fresh 2FA.

## The bot's captcha picture was missing from every preview (2026-11-19)

`discord.rs::embed` loads only `https://` images (so a typed address cannot learn the viewer's IP). The bot's own captcha sample is a `data:image/png;base64,` address, so the picture was silently dropped in the wizard preview (the Verification page only showed it through a plain `<img>` fallback).

**Rule:** the preview's image filter also lets `data:image/png;base64,` through (it makes no request). Any new sample with an embedded picture must use that form, never another `data:` type. Preview the real message: a missing part of a bot sample is a filter, not a missing sample.

## A switch in the sidebar is no weaker way in (2026-11-19)

The open module's On/Off switch in the sidebar (`modules::ModuleSwitch`) saves with the same `PUT /config/<section>` as the General page, so the server's access and 2FA rules apply to it. It sends the module's whole stored section with `enabled` flipped, never a partial object (the PUT replaces the section).

**Rule:** a new module switch uses `switch_section` and `ModuleSwitch`; never add a separate route for it. The switch sits inside the module's link, so its click calls `prevent_default` and `stop_propagation` and the checkbox follows only the saved config. Turning off asks first (`ask` with `.danger`); the toast, busy flag and 2FA state live in `App`, because the nav is rebuilt when the config changes.

## The landing page showed for a moment after every login (2026-11-20)

`App` drew the landing page as the fallback while `me` was empty, and `me` stays empty until `/api/me` answers. After the login callback sent the browser to `/`, the landing page was drawn first, then the server list replaced it once the session was read.

**Rule:** until `/api/me` has answered, `App` shows only the loading sign (`checked`). Set `me` before `checked`, or the landing page shows between the two sets. Any new "logged in or not" view waits for that answer too.

## After the 2FA code, some actions asked to be pressed again (2026-11-20)

The prompt repeated an action by clicking the control the click landed on again. That failed in two ways. (1) A request sent after an await has no click to repeat (the click record is cleared at the end of the click): Overview Fix all sends its fixes one after another, so a step-up that ran out in the middle of the run opened the prompt with nothing to resume, and the toast said "press the button again"; a second press then ran the whole list again from a list read before the run. Config import (after the file read) never resumed. (2) A control that is gone or drawn again is not in the page any more: the Apply import button, the Channel Setup push dialog, the Delete server data dialog and the Load backup dialog close before their request; the panic mode button shows the other action by then.

**Rule:** a request that starts after an await, or whose control closes, hides or changes, uses `run_resumable` (or `run_change_resumable`): it gets a closure that builds the same request again, and the prompt runs it once after a good code (dropped on Cancel and on a page change). A run of several requests keeps what is left and goes on from there. A plain click handler that keeps its control on the page can stay on `run_act`. The wizard card's setup request (`form.rs` `act`, also sent after a save) is resumable too. A refusal that comes back after the user opened another page opens no prompt. Where nothing can be resumed (the Channel Setup draft auto-save puts the server's draft back on a refusal), the prompt says "Press the button again to finish", never a bare "Verified.". The server checks the step-up and CSRF token on the resumed request as on any other: nothing is stored outside the page's memory.

- Server card without banner (2026-11-20): Discord does send `banner` in the guild list, but only servers with a banner (boost level 2) have one; most have none, so cards showed an empty gradient. Fall back to the zoomed, blurred icon (clip it: `overflow:hidden` on the parent). A button in a flex row needs `flex:none; white-space:nowrap`, or a long name wraps it.

## A new table class took a settings card's layout rule (table card, 2026-11-20)

The table card's button column was first called `.act`. `style.css` already has a bare `.act` rule (the settings card's action row: flex column, 8 px top margin), so the Permits Edit cells became flex boxes and the Actions column sat lower than the Name column. Only the desktop screenshot showed it; the e2e sweep passed.

**Rule:** before naming a new class, grep `style.css` for a bare rule of that name (`\.name\b`). Same cause as "A chip class name took a popup's layout rule" above. The table button column is `.tact`.

## A grey frame around the Backups settings (layout PR 6, 2026-11-20)

The Backups page wraps its settings in a `<fieldset>` so a viewer below the owner level gets them read-only. Only `fieldset.ro` had its border and padding removed, so for the owner the browser's default fieldset frame (2 px groove, padding) drew a box around both settings cards. The e2e sweep did not catch it; only the desktop screenshot showed it.

**Rule:** a plain HTML element that the browser styles (fieldset, legend, details, button) gets its base reset in `style.css` once, for every state, not only in the class one state adds. `fieldset` now has no frame by default; the e2e sweep fails on a framed fieldset on Backups.

## A detail page kept its own big title and back button (Backups, layout PR 6, 2026-11-20)

One backup opened inside the Backups page from a signal: the page drew a second `h1` (the backup code) and a "Go back" button under the page header, against the rule that a detail page extends the crumb. A detail held in page state cannot reach the crumb, which `main.rs` builds from the route.

**Rule:** a detail view is a route (`#/<server>/backups/<code>`, `#/<server>/cases/<no>`), and `main.rs` adds it to the `detail` crumb list; leaving it is the crumb link, and a delete goes back with `set_hash`.

## Everything looked tiny on a big monitor (owner decision, 2026-11-20)

The dashboard used fixed pixel sizes, so on a 2560 px monitor the page was a small column of 14 px text.

**Rule:** the screen scale is one `zoom` on `#app` (`--z` is set on `:root`), stepped by window width through `--z` (`design/README.md`, "Screen scale"). Chrome and Firefox do not zoom `vw` and `vh`, so a bare `100vh` is `--z` times too tall; use `calc(N * var(--vh))` / `var(--vw)`. Window coordinates (`getBoundingClientRect`, `client_x`, `scroll_y`, `inner_width`) are real pixels while a `left`, `top` or `--dx` set from them is a zoomed length: divide by `screen_zoom` (`common.rs`) or the popup lands `--z` times too far (tooltips, the message editor's emoji and variable menus). Ratios inside one rectangle (the colour picker) are fine. `tools/ui-sizes.mjs` fails on a bare `vw`/`vh` and on a second `zoom` in `style.css`.
## Each list drew its own hover and drag look (sortable list, 2026-11-20)

The saved messages list had an orange outline on hover, Channel Setup a background only, and Permits up and down buttons: three looks for one idea, and orange on hover said "selected" when nothing was. Native HTML drag also does nothing on a touch screen, so a whole-row drag can not be the phone way.

**Rule:** a sortable or selectable row takes its outlines from `--ol-hover` (grey) and `--ol-pick` (orange) in `style.css`, and a new sortable list uses `.slist` / `.srow` with `Grip` from `common.rs` (mouse: native drag on the row; finger: pointer events on the grip only). A confirm dialog opened from another dialog comes after it in the page, or it opens under it.

## The Add feed dialog: both choices orange, a cut-off help bubble, a stray line (Feeds, 2026-11-20)

Four faults from one screenshot, three root causes.

1. **Both source buttons were orange.** `.seg` (the two-choice selector) had no rule of its own: every page scoped it (`.cs .seg`, `.mcard .seg`, `.pop .seg`, `.send .seg`, `.chpick .seg`), so a page outside those parents (Feeds, with its sort control too) got plain orange buttons and no "open" state. **Rule:** `.seg` has a base rule in `style.css` (muted buttons, the open one orange); a scoped rule may only add to it. Any new `class="seg"` works anywhere.
2. **A help bubble was cut at the dialog's edge, and a thin line showed along its bottom.** `.modal .card` scrolls (`overflow:auto`), so an absolutely placed `.tip .bubble` is clipped by the card and widens its scroll area (the line is the horizontal scrollbar). `Tip` placed the bubble inside `main`, which is wider than a dialog. **Rule:** `Tip` measures against the dialog card when it sits in one (sideways and upward), else the page area. Checked: every `Tip` in a dialog (Feeds, Commands settings, Permits, Channel Setup push) uses it.
3. **A grey "Type the YouTube channel." line read like an error and moved the buttons.** A missing field is said once, on the field's own hint line and on the disabled button's tooltip, never as a line between the form and its buttons.

**Also:** the add form asks for one link. `guess_kind` (`feeds.rs`) names the source from it (YouTube @handle, channel id or youtube.com page; github.com; gitlab.com; else RSS). YouTube's own `/feeds/` links are RSS. A self-hosted GitLab cannot be told from a website, so "More options" keeps a Source list (Detect from the link, RSS or Atom, YouTube, GitHub, GitLab). The server still gets an explicit `kind`, so no safety rule changed.
## Fields that a password manager takes for a login

1Password (and LastPass, Bitwarden, Dashlane) guess a login form from field names, search boxes and code fields, with no password field present. It then offered "Sign in" on most pages (Bot Profile has a "Bot name" field and a search box). The dashboard has no login form: sign-in is Discord OAuth.

**Rule:** do nothing per field. `index.html` `quiet` gives every `input` and `textarea` that mounts `autocomplete="off"` (unless it sets its own) plus `data-1p-ignore`, `data-lpignore`, `data-bwignore` and `data-form-type="other"`. The authenticator code field keeps `autocomplete="one-time-code"` so a manager can fill its code. Never add a `type="password"` field. The Rescue Key is shown as text and claimed in Discord, so no dashboard field holds it.

**Check:** the e2e sweep fails on any field without those attributes. The real test is a person with the manager installed.

## The front page hero must not reuse a dashboard class name (front page hero, 2026-11-20)

The first hero draft named its glow circle `.ring`, which is the dashboard's score ring (`.ring` in `style.css`), so the circle came out stretched into an oval. The dashboard's element and class rules are global and the front page shares the stylesheet.

**Rule:** front page classes start with `hero-` or `land-` (`.land .hero-...`). Before adding a class, grep `style.css` for it. Preview a front page change with the real page (`trunk build`, serve `dist/`, answer `/api/*` with 401), not a static copy, since a static copy does not load the dashboard's rules.

## Channel Setup could not move a channel on a phone (owner decision, 2026-11-21)

On a touch screen Channel Setup turns native drag off (it does nothing there) and the ⋮ menu's "Move to category…" only selected the row and called `focus` on the category picker in the side panel, one animation frame later. iOS Safari ignores `focus` that does not run inside the tap itself, and on a phone the panel sits under the whole list, so the tap did nothing visible. The Roles tab had no finger way to reorder at all.

**Rule:** a menu item does its job itself; it never only moves the focus to a control somewhere else. The ⋮ menu now holds the category picker (`<select>` opens from the tap). A list with a selected row (Channel Setup channels and roles) uses `HoldDrag` (`common.rs`) for the finger: tap selects, a press held still for 0.4 s on the selected row lifts it, a finger that moves first scrolls. Its places carry `data-drop`. A menu on one of the last rows opens upwards (`.rowmenu.up`), or the push bar covers it.

## A refresh button must refresh what the person is looking at (server refresh, 2026-11-21)

The sidebar's "refresh server" button only fetched `/api/guilds` again. The server list changed nowhere visible and the open page never fetched its data again, so a tap did nothing a person could see. The button was also a green pill set absolutely inside the server link (`<a><button>`), which is invalid HTML and sat out of place on top of the name.

**Rule:** a refresh control refetches the data on screen (main.rs `refresh`: the server list, then `page_tick` builds the open page anew) and asks first when a save bar (`main .bar`) shows unsaved edits. A failure shows in the toast. Never put a button inside a link: put it next to the link in a row (`.srv-row`).

**Check:** in the browser, count the `/api/` requests after the tap: more than the server list must go out, and the page content must load again.

## A button that sends a request twice on a double click
- **Cause:** a button whose handler starts a request (a queued job, a post, a key, a token) but never goes busy. The confirm dialog does not help: it runs its action once, but the first button is pressable again at once and opens it again. Found in recovery.rs, pages.rs, tickets.rs, lockdown.rs, secure.rs, main.rs (history), layout.rs, embeds.rs (Make channel), records.rs, mcp.rs, raiders.rs, appeals.rs, datareq.rs.
- **Rule:** wrap the request in `guard(busy, fut)` (common.rs; sets `busy` at once, clears it when the request answers, also on a refusal or a 2FA prompt so the code's retry can click again), then `disabled=busy` and `busy_label(..)` on the button. A list row uses a signal made in its own closure (or a `(id, key)` pair for the row's several buttons). A queued bot job (`recovery.rs queued`) keeps `busy` on until its reload, so the same job is not queued twice before the bot has taken it.
- **Server side (not fixed here):** the handlers are not idempotent. A repeated request queues a second backup or load, mints a second Rescue Key (the first key is dead and only the last answer on screen is right), posts a second ticket panel or makes a second MCP token. Purge, Secure my account and the decisions stop themselves on the second try. Only a script or a second tab can send the repeat now.
- **Check:** a new button that starts a request needs `disabled=busy`. Click it twice fast in the browser and count the requests.

## A slow action needs a pending state
- A button that starts a slow request (the server asks GitHub first) must show the working sign and be disabled from the click until the page has the result. `update.rs` first showed nothing for 2-3 s; use `busy_label` and set busy before the request. Clear it on a refusal (so a 2FA retry can click again), and keep it until the next answer shows the queued run.
- A failed poll while the dashboard restarts itself is expected: show the same page (text only), never a pose. A pose is a state of its own (empty, error, offline), not a loading flicker.

## Screen scale stops at 1920
- `zoom` on the root above 1.25 (the old 1.45 to 2.45 steps) made a 2560 px window look zoomed in: text, controls and cards all grew. The scale stops at 1.25 from 1920 px up; more width is margin. `tools/ui-sizes.mjs` fails on a `--z` above 1.25.
- Wide windows (2026-11-21): above 1920 px the width goes to columns: `--w-wide` widens (1600 px from 2300, up to 2300 px from 3200) and `.cards` becomes CSS columns (3, then 4). Cards flow down the columns, so a card that grows (a "+" list gets an item) can push the next card into the next column; it is by design on wide screens only. Never put a page with its own grid under `.cards`.
- A label with its field beside it outside a `.row` needs the `field` class, or the text touches the input.

## Signals after an await
- `try_set` gives the value back (`Some`) only when the signal is gone, so "page gone, stop" is `if busy.try_set(false).is_some() { return }`. The Feeds page had `is_none()`: each feed action stopped on success, so the dialog stayed open, the list did not reload and no message showed (2026-11-21). `try_update` is the other way round: `Some` means it ran.
- A signal made in a click handler belongs to the view that drew the button. When that view draws again (channel names arrive, a list reloads), the signal is gone while a dialog still reads it, and the page panics. Make the dialog's draft signal once in the component and set its value on open (`feeds.rs` `open_dialog`, the `open_editing` of Sticky, Auto Responder and Triggers).

## Wide columns, loading names, mention text (2026-11-21)
- The wide-window column rule set the card gap with `.cards .card`, but the later `.home .card { margin:0 }` has the same weight and won, so Home cards touched in 3 and 4 columns. Page rules that reset card margins win over a shared rule of equal weight: the shared rule uses `main .cards .card`.
- A form drawn before `/discord` answered showed every set channel and role as "none" (and empty role lists), which reads as "not set". Load the name list before the fields; a set id missing from the list shows "not found (deleted, or not loaded)", never "none"; a failed name list shows an error.
- Text with Discord mentions from the server (`<#id>`, `<@&id>`) is shown with names from `/discord`, never as raw ids (Lockdown check). Load the names before the list.
- A request with no error branch left its loading line forever (Home Overview card on a failed score). Every loading state needs an error state.
- `md::inline` does not draw `{e:name}` tags; embed titles and field names in previews need `icons::fill_tags` too.
- A class used for two layouts (`.facts` as a `dl` grid and as the Details/stat row) broke the second: the grid rule pushed Overview Details off a phone. Scope such rules by element (`dl.facts`).
- The Update page showed "Update now" when the newest build was unknown (GitHub not reachable). A button shows only when its action can work: Update now needs a known newer build. The server refuses such a start too.

## Feed rows (2026-11-21)
- **Row buttons moved with the text.** `<span class="sp">` grew only inside parents with their own `X .sp { flex:1 }` rule; the Feeds row (`.frow`) had none, so Edit and ⋯ sat right after the text and jumped when the status line changed. *Avoid:* `.sp { flex:1 }` is one global rule; a spacer works in every flex row. A row's controls sit after the `.sp`, and the text block has `min-width:0` so long text wraps before it pushes them. On a phone the row is a grid (owner's layout): icon, title (with its chip, in `.ftitle`) and switch on the top line, the details and error lines over the whole width (`.fmain` is `display:contents` there), ⋯ and Edit on the last line at the right; a `<details>` in a row needs `margin:0`, or the page's `details` margin adds space.
- **An untouched default showed "{link} is missing".** The editor's `botmsgs::missing` read only the first embed's text, but the GitHub look keeps `{link}` as the title's link (`url`), which the server allows for `feed.*` entries. *Avoid:* the UI check follows `botmsg::missing_text` exactly; `every_default_passes_its_own_check` proves each default passes the server check, and `a_feed_link_may_be_the_title_link` the UI one.
- **"last post in a minute ago".** `relative()` takes the time from now and adds "in" or "ago" itself; Feeds passed now minus the time and added "ago" again. *Avoid:* pass `time - now`; a past time is clamped below 0 so a slow clock never reads "in".
- **"Asked" stayed on a row.** The row line for a requested check showed until a reload, and a webhook feed turned on kept `asked = 'check'` for ever (nothing checks a webhook feed). *Avoid:* the page reads the list again every 10 s while a feed waits; the server sets a check only on feeds it reads and hides `asked` on webhook rows.

## Channel Setup list is compact; set-up buttons (2026-11-21)
- The owner wants the Channel Setup channel list compact. Its row rules (lines that start with `.cs .tr`, `.cs .slot` or `.cs .nm` in `style.css`) do not follow the 4 px scale or the padding tokens; `tools/ui-sizes.mjs` skips only those lines (`CS_LIST`). Do not make these rows larger again to fit the scale. Other Channel Setup rules (panel, dialogs, toolbar) still follow it.
- The rows were 44 px high because the global `details { margin:12px 0 }` also hit the row's "⋮" menu (`details.rowmenu`). A `details` used as a small control inside a row needs `margin:0`.
- The embed image `.slot` rule (dashed border, background) also matched Channel Setup's drop slots, so a dashed line showed between groups at all times. A Channel Setup slot shows only while a drag runs (`.cs .slot:not(.on)` resets it). A short shared class name needs a scope.
- A "Set up" or "Create" button shows only when its action has something to make: Catch Channel hides "Set up channel" while the catch channel is set and exists in Discord, Logging hides "Create a channel per log type" while every log type has a channel (`GET /logging/setup` gives `todo`, from the same `log_gaps` the setup uses), and a wizard card's action hides while its `done` channel exists. Tickets already did this. A new set-up button follows the same rule.

## One page width (owner decision 2026-11-21: "I want width to be equal ... go with wide ... universally")

**What went wrong:** pages had three widths. Pages of Cards (and Channel Setup) widened through `main:has(.cards, .gcards, .cs)` from 2300 px; other pages stayed at 1200; lists, `details.how`, `.mcard`, `.feeds` and `.stack` used a 900 px `--w-narrow`. So a page jumped wider when its cards finished loading (the loading state has no `.cards` yet), and the "How it works" box was narrower than the cards under it.
**Rule:** the content width is one token, `--w-wide`, set only on `:root` and stepped only by window width (1200 px, 1600 px from 2300, up to 2300 px from 3200; all at scale `--z`). `main` uses it; every block inside a page (head, How it works, notices, stacks, lists, cards) fills `main`. Never set the width from page content (`:has(...)`), never give a page-level block its own `max-width`. Only dialogs, previews, fields and small forms inside a block may be narrower. The server list has no sidebar, so it is wider than server pages below 1500 px; that is the same token, not a second width.
**Check:** at 1366, 1920 and 2560 every server page's `main` content box has the same width, and its direct children fill it, also while the page loads.

## The small sidebar switch rule never applied (2026-11-21)
- `nav a .sw` and `.atit .sw` set a small switch (36 x 20 px), but `input[type=checkbox].sw` has a higher specificity, so the full 44 x 24 px switch showed. In the sidebar, the switch made its row 3 px higher than the other rows.
- **Rule:** a rule that changes the switch size uses the same selector form: `nav a input[type=checkbox].sw`. To check, measure the row heights: every sidebar module row has the same height, with or without a switch.
- Every module with a single on/off (Auto Mod, Anti Nuke, Tickets) shows its switch in the sidebar, open or not. A part with its own on/off shows its switch on the module page. More than one switch is in the sidebar now: a test selects a switch by its `aria-label` ("Anti Nuke on or off"), not by `nav input.sw`.

**An extra panel went live as a "New panel" button.** (tickets, 2026-11-22)
"Add panel" gave every panel after the first the placeholder label "New panel" and no emoji. Staff often save without a rename, so members saw "New panel" in #open-a-ticket, and a re-made channel posted it again. **Rule:** a default that the bot posts is text members can read as is ("Open a ticket"), never a placeholder. New panels carry no category, so their tickets go to the ticket category. Migration 4057 gives a saved panel that still has the placeholder ("New panel", no emoji) the default button and drops its category; the bot posts the panel message again at start.

## The front page top bar never showed on a short screen (2026-11-22)

The bar is hidden until the visitor scrolls past the hero's copper line. The first test, "the line has left the top of the screen", never fired on a 1366x768 screen: the page ends 418 px below the top, so the line stays 350 px down.

**Rule:** a scroll trigger that depends on a position must first check that the page can scroll that far. `landing.rs` shows the bar at the line, or at half of the scroll range when the line is out of reach. A hidden bar uses `visibility:hidden`, so it is not focusable; do not use opacity alone.

## The front page bar broke the size check (2026-11-22)

The new top bar rule wrote `padding:... calc((100% - 992px) / 2)`. `tools/ui-sizes.mjs` allows no raw pixel value in a padding, so dashboard-ui failed on main. It was not run before the push.

**Rule:** a width you need inside a padding is the page width token `--w-wide` (never a raw value or a new property that only hides it), and `node tools/ui-sizes.mjs` runs before every push that touches `style.css`.

## Module and part switches lied after a failed reload and on an off module (bug hunt 2026-11-22)

- After a good save, the switch read the config again and ignored a failed read: it kept the old state and the toast said "Saved!".
- A part of an off module (Join Gate while Server Joins is off) showed ON and asked "Turn Join Gate off?", but nothing ran. An Editor got an enabled part switch there, and every click was refused, because turning the module on needs a Trusted Admin with 2FA.
- At 375 px a part card held icon, text, switch and chevron; the title wrapped in an 80 px column and the chevron left the card.
- Root cause: the switches were tested only on the good path, as Owner, at desktop width.
- **Rule:** a control that reads the state again after a save reports a failed read as an error. A part switch shows what runs (`part_on` reads the module too), and its `may` asks for every bar the server checks for that write. A card row that adds a control is measured at 375 px.

## A dead session, a failed load and level none showed false pages (bug hunt 2026-11-22)

- A session that ended while a page was open (expired, idle, deleted) kept the signed-in shell; Cases said "login required" next to "No cases found.", with no way back to sign-in.
- Six list pages set `loaded` after a failed load too, so the empty-list text ("No cases found.", "No backups yet.", "No open tickets.") showed under the error as if it were real data.
- A failed access answer showed a Viewer every page (it counted as Owner).
- `/api/me` failing (500, no connection) showed a signed-in person the public front page. An access answer of 200 with no body left the page on its skeleton. A signed-in member with no level got Home with "Open the wizard".
- Root cause: the pages were tested only with a live session and good answers.
- **Rule:** `reply` sends a 401 on a signed-in page (CSRF set) to sign-in with `next`; logout and Secure my account call `signed_out()` first, so a late 401 does not sign the person in again. A list sets `loaded` only after a good load. Only "login required" from `/api/me` is the front page; other errors say so. An access answer counts only with its `guild`; a failed one fails closed (`failed`: no page, rank 0, a Try again card). Level `none` sees no page and no Home.


## Leaving with unsaved edits (2026-11-22)
- One guard for the whole dashboard: `guard::guard(dirty, busy, problem, save)` in the page, `guard::leave(then)` for a page's own Back button. Never write a page-local "Leave without saving?" ask. `problem` is the text of what must be fixed first (a missing `{text}`, a JSON error); the dialog then has no Save button.
- A page change is held in `guard::hold` (main.rs router): the address is put back with `replaceState`, the dialog opens one tick later (the dialog closes itself on a hash change). Do not create an `Effect` in a dialog answer: the dialog's view owns it and drops it when the dialog closes; wait with a timeout loop (`settle`).
- Tickets and Logging are hubs like the other modules: parts are NAV keys like `tickets/publish`; `Access::of` reads the part before the `/`.

## One searchable picker for channels, roles, categories and members (2026-11-22)
- A channel, role or category is chosen with `form::ListPick`, never a native `<select>`, a plain text or ID box, or a `<datalist>`. Many of them are chips plus the same `ListPick` (`channels`/`categories`/`roles` kinds). A member is chosen with `UserPick` (`form::one_member` for a single one). "Pick, then Add" controls keep the pick in a signal, not a `NodeRef` to a select.
- Root cause of the empty Channel box on Publish panels (v1.2.1): `ListPick` was an `<input list>` with a native datalist. Firefox shows no list on focus or click, only after a key is typed, and the box showed no arrow, so the field looked like an empty text box. Chrome opens it, so the fault was not seen in tests. `ListPick` now draws its own list (opens on focus, filters as you type, arrow keys, Enter, Escape), so every picker is fixed at once.
- **Rule:** a new field that takes a channel, role, category or member uses these pickers. `tools/ui-sizes.mjs` fails on a datalist input (only the time zone box in `msgrules.rs` may keep one) and on a `<select>` for a channel, role or category. `tests/e2e/dashboard.js` opens the Publish panels picker, filters it and clears it. A list is drawn by the page itself, not by the browser, because browsers differ on native lists.

## The picker list was cut off inside a dialog (2026-11-23)
- Root cause: the open list of `ListPick` was `position:absolute` under its box. A dialog (`.modal`, `overflow:auto`) or a card with hidden overflow clips or scrolls it, so on the Feeds "Change" dialog only three rows showed and the owner had to scroll the dialog to see the rest.
- **Rule:** the list is `position:fixed` (`.lpl`), placed from the box's rectangle every animation frame while open, so it follows any scroll or resize. It opens below the box, or above when there is less than 160 px below, and its height is capped to the room on screen; it scrolls inside (`overscroll-behavior:contain`, so the wheel does not move the dialog). The page is scaled by `--z` zoom on the root, so offsets are divided by the ratio of the rectangle width to `offsetWidth`. Escape closes only the list while it is open (it does not reach the dialog). A new popup list uses the same pattern, never `position:absolute` inside a dialog. `tests/e2e/dashboard.js` opens the Feeds dialog picker at desktop and phone size and checks the list is inside the window, on top and next to its box.

## The release browser test waited for a page the leave guard held (2026-11-22)
- Root cause: `tests/e2e/dashboard.js` changed the Verification page ("+" on the captcha preview) and then did `page.goto` to the Messages page. That is only a hash change, so the new leave guard (`guard::hold`) put the address back and asked "Unsaved changes". The Messages page never opened and "Add new message" timed out in the v1.2.0 release run. The e2e job runs only on a tag, so main stayed green.
- **Rule:** a step in `dashboard.js` that leaves a page it edited goes through the shared `page.goto` wrapper, which answers "Discard changes" when the dialog shows. A new page guard needs no test change; a page that keeps edits on purpose must be tested with its own click.

## Front page: switching a line moved the page

Each scene set the stage height (`--sh`) when it was the active one, so a taller or shorter scene changed the height of the block and the lines and the page below jumped. A hover lift on the pieces and a bold active line made the page feel loose too.

**Rule:** the stage is as tall as the tallest scene of its block; a switch changes only which scene shows. Do not change the weight, size or height of anything a click on a line touches. Do not use `prefers-reduced-motion` to turn off front page motion: motion is always on (owner decision).

## Front page: stars placed against one scene covered another

Stars placed against the group of the scene that was open covered text in other scenes of the block, because scenes of different size sat at different places in the shared box.

**Rule:** the stars are fixed in the block's box. All scenes of a block start at the same corner, and the star places come from the sizes of the first pieces of all scenes (`fit` in `landing.rs`). After a change to the scene layout, open every scene of every block on a desktop and a phone width and check that no star covers text and no scene leaves the box.

## Icons on coloured buttons lost their contrast

The emoji pictures got an orange gradient (`fill="url(#name-grad)"`, `stroke` the same). The dashboard draws the same SVG through `icon`, so the backup page's green Load and red Delete buttons, and the green and red arrows of the setup wizard, showed orange on green and on red. The icon was hard to see.

**Rule:** on a filled button an icon takes the button's text colour. `style.css` sets `fill` and `stroke` to `currentColor` for the gradient parts under `button.go`, `button.danger`, `a.btn` (not ghost) and `.wiz .nav`, as the active menu link already did. A new filled button style with an icon joins that list. Check: a browser sweep of every page found no other filled button that still shows the gradient.

## A password manager's button sat far from the 2FA code field

The screen scale was a `zoom` on the page root. A password manager adds its own button to the page and places it from the field's `getBoundingClientRect`. That rectangle is already scaled, and the button sits under the zoomed root too, so the browser scaled its place a second time: at scale 1.25 the button landed about 25% too far right and down (seen in Firefox, measured the same in Chromium).

**Rule:** the zoom sits on the app's own box, `#app` in `index.html` (the app mounts there, last in body), never on `html` or `body`. Anything a browser extension adds to the page then keeps real pixels. Check: a browser sweep of every page at three widths in Chromium and Firefox showed no change; a button placed from a dialog field's rectangle lands on it.

## A question box opened behind the dialog that asked it

`ask()` (common.rs) drew its box with the same z-index as page dialogs, and it comes earlier in the page, so a dialog that opened it covered it: "Save as template" showed no name box, and the page looked frozen.

**Rule:** the box of `ask()` has the class `modal top` (z-index 55, above dialogs, under the toast). A new overlay that can open from inside a dialog uses `top` too. Run the flow in a browser to the end: a click that works in a test with force can hide a box nobody can see.

## A call to the list URL ended with a slash

`act(String::new(), ..)` built `{base}/`. The server routes the list and start on `{base}` and answered 405, so Start from a form did nothing. One helper now leaves the slash out when the path is empty.

**Rule:** build an API URL from parts with one helper that never leaves a trailing slash, and let the browser test press the button that starts the thing.

## A long variable chip ran out of the editor box

A chip had `white-space:nowrap` and no width limit. A long variable name pushed the chip under the next column, and on a phone it was cut at the edge of the screen. The chip now stays inside its box, cuts with an ellipsis and keeps the full text as its title.

**Rule:** an inline part that cannot wrap needs `max-width:100%` and an ellipsis. Check it in the narrowest column and at 390 px.

## A preview drew only the part that can be edited

The preview drew the saved message. Buttons that Brim adds when it posts (Enter, Claim, links under logs) were missing, so the owner could not see the real message. A wrong example for a variable (names where a running giveaway has a number) was not seen either.

**Rule:** a preview shows the whole message a member gets. Parts that Brim adds come from the bot's own builders (`crates/bot/src/previews.rs`, `added_buttons`), never from a copy in the page. They carry the shared notice and cannot be pressed. Give a variable the example that fits each message, not one example for all.

## Revert and Undo set the message without the parts Brim adds

The revert path replaced the whole editor value with `{"body": ..}`. The `added` rows (buttons Brim adds) were lost, so the preview showed less than the bot posts until the page was reloaded.

**Rule:** change one field of the editor value (`cur.update(|c| c["body"] = ..)`), never rebuild the whole value in a later path.

## One example value for a variable that means different things

`{text}` had one example, "What happened, written by Brim.", in every message. In the ticket panel it read as a panel line, in a ticket reply as a member's words, which is wrong.

**Rule:** when a variable name is shared by messages that fill it with different things, give each message its own example (`var_info` in `crates/logging/src/botmsg.rs`), and take it from the server's real data where the dashboard knows it (the ticket panel lines).

## A long link in a Discord preview ran past the message box

A message line with a long link had no break point, so on a phone it ran outside the box. Discord breaks such a link.

**Rule:** the text column of `.dmsg` has `overflow-wrap:anywhere`. Check a preview at 390 px with a long link in it.

## The browser's own undo does not know the message

Ctrl+Z in a text box only undoes typing. An added field, a changed colour or a removed embed changes the message, not the box, so the browser skipped it. A box that was drawn again (a different message loaded) also lost its undo steps.

**Rule:** undo and redo go through `Editor::step` in `embeds.rs` (steps of the message body). Keys and the browser's own undo (`beforeinput`) both end there. A place that loads another message into the editor calls `Editor::reset`, or Undo would bring back the old message. Leave Alt out of shortcut checks: AltGr is Ctrl+Alt on many keyboards.

## The tab row has one sliding bar (all tab rows, 2026-11-23)

`PageTabs` (`common.rs`) measures the open tab and sets `--tx` and `--tw` on the row. `.tabs::after` is one orange bar that moves with `transform` only, and the content under the row fades in with opacity (`tf-a`, `tf-b`). Without the measure the open tab keeps its own orange border.

**Rule:** do not draw a second underline on a tab row, and do not animate left, width or height there. A route tab builds the row again on every page, so the last place is kept in `TAB_AT` to slide from. A test that checks the orange line reads the bar when the row has `data-slide` (`tests/e2e/dashboard.js`).

## A glow and a clip-path on one element (front page stars and hover line, 2026-11-23)

The star glow was a `filter` on the same element that had the star `clip-path`. The clip runs after the filter and cut the glow away, so no star glowed. The star shape is now on `::before` and the glow on the element itself.

The hover line of the big buttons is a layer behind the button (`::before`, `inset` -3 px, radius plus 3 px) and the button covers its middle. A ring made with a mask beside the button left a gap and a hairline.

**Rule:** to light the edge of a shape, put the shape on a child and the filter on the parent.

## Hover and focus lines come from the element's own geometry (2026-11-23)

A hover line, ring or highlight is a layer ON the element it marks. It is never a separate shape placed beside it.

- Wrong: a second box, mask or border drawn next to the button with its own size and radius. It drifts, leaves a gap or hairline, and is not concentric.
- Right: `position:relative` on the element and a `::before` with `inset:-<width>`, `border-radius:calc(<the element's radius> + <width>)`, `z-index:-1`. The element covers the middle, so the line shows only as an even edge and cannot gap. Take the radius from the same token as the element.
- A hover line must not change layout, so use `inset` and no border or margin.

Two causes of a clipped ring (found in the button audit):
1. A parent with `overflow` (`main { overflow-x:clip }`, `nav { overflow:auto }`) cuts a ring drawn outside the element. Give the parent `overflow:visible` where it is safe, or draw the ring inside with a negative `outline-offset`.
2. `clip-path` on the element cuts its own `filter` glow (see above).

After any change to a button, Tab through the page and check each focus ring is complete on all sides.

## Front page: the switch ended with a jump (2026-11-23)

The owner saw the new scene sit a little to the right while it moved, with no real fade, and jump when it stopped. Recorded in Chromium (page paused at 299.9 ms of 300 ms, compared with the finished page): about 20,000 pixels differed, the text sat about 1 px off. Cause: a scene is placed with `translate` and `scale` set to fractions (the fit values `--dx` and `--k`). While the scene animated it was its own layer, drawn at a fractional position; when the motion ended the browser dropped the layer and drew the scene again, so the text snapped to a pixel. `getBoundingClientRect` showed no difference, so a position check passed. The fade was `ease-out`, which brings the new scene to 74% in the first 40% of the time, so the cross-fade looked like a pop.

**Rule:** a motion ends exactly at the resting layout. A moved piece keeps its layer when it rests (`will-change:transform,opacity` on `.feat-shot > .scene`), so the first resting frame is the last animated frame. The cross-fade is linear, so the two scenes always add up to one. Check a motion by recording (pause it at 0, 25, 50, 75 and 99.9% and compare the pictures; the page at 99.9% must equal the rested page), never from still screenshots or from rectangle numbers alone.

**Update (2026-11-23):** the owner removed the hover line of the big front page buttons and then the white pressed look. They now show nothing on hover or when pressed except the 97 % press-down; the `::before` layer is gone. Do not add a hover or pressed highlight to them again. The rule above still holds for any other outline.

## A pressed button turned white under its label (front page, 2026-11-23)

The white layer of a pressed front page button (`::before`, `z-index:-1`) lay over the button's own fill, because the press transform makes the button a stacking context. Light labels became white on white. Fixed by removing the layer.

**Rule:** a state that changes a button's fill sets the label colour with it. Look at the pressed, hover and focus still of every button style in both browsers before reporting.
