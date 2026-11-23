// Logs into the dashboard through the real OAuth callback (against mock_discord.py), then opens every
// sidebar page, failing on console errors/panics or blank pages. Screenshots go to $E2E_OUT.
const { chromium } = require('playwright');
const base = process.env.DASHBOARD_URL || 'http://127.0.0.1:8080';
const mock = process.env.DISCORD_API_BASE || 'http://127.0.0.1:9100';
const out = process.env.E2E_OUT || '/tmp/e2e';
const fs = require('fs');
fs.mkdirSync(out, { recursive: true });

(async () => {
  const browser = await chromium.launch();
  const ctx = await browser.newContext({ viewport: { width: 1400, height: 900 } });
  await ctx.addCookies([{ name: 'oauth_state', value: 's', url: base }]);
  const page = await ctx.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(`pageerror: ${e.message}`));
  // The dashboard gives a session 40 API requests at once and 20 more each second (auth.rs BURST and
  // RATE) and answers 429 past that; a page whose first requests get a 429 stays empty. These steps
  // open pages faster than a person does, so every page load first waits exactly until the limit
  // has room for it (a count of this page's own requests, no fixed sleep).
  let tokens = 40, at = Date.now();
  const refill = () => { const now = Date.now(); tokens = Math.min(40, tokens + ((now - at) / 1000) * 20); at = now; };
  page.on('request', (q) => { if (q.url().includes('/api/')) { refill(); tokens -= 1; } });
  const goto = page.goto.bind(page);
  page.goto = async (...a) => {
    refill();
    // 15: a page load sends up to about 10 requests.
    if (tokens < 15) await page.waitForTimeout(Math.ceil(((15 - tokens) / 20) * 1000));
    const r = await goto(...a);
    // A page with unsaved edits asks first (guard.rs); these steps never keep edits, so they discard.
    await page.locator('[role="dialog"] button:text-is("Discard changes")').click({ timeout: 600 }).catch(() => {});
    return r;
  };
  page.on('console', (m) => m.type() === 'error' && errors.push(`console: ${m.text()}`));
  page.on('response', (r) => r.status() >= 500 && errors.push(`${r.status()} ${r.url()}`));

  await page.goto(`${base}/auth/callback?code=x&state=s`);
  await page.waitForSelector('a.gcard', { timeout: 15000 });
  await page.screenshot({ path: `${out}/servers.png` });
  await page.locator('a.gcard').first().click();
  await page.waitForSelector('nav a, .side a, aside a', { timeout: 15000 });
  // Wait for the thing the test reads next: the sidebar's module links of this server. A count of all
  // "#/" links was met by the header's links alone, before the sidebar had rendered.
  await page.waitForFunction(() => [...document.querySelectorAll('a[href^="#/"]')].some((a) => /^#\/\d+\/module\//.test(a.getAttribute('href'))), null, { timeout: 15000 });

  const pageLinks = async () => [...new Set(await page.$$eval('a[href^="#/"]', (as) => as.map((a) => a.getAttribute('href'))))].filter((h) => /^#\/\d+\//.test(h));
  const failed = [];
  const links = await pageLinks();
  // The sidebar lists modules; each module page lists its parts (rows with a link).
  for (const m of links.filter((h) => h.includes('/module/'))) {
    await page.goto(`${base}/${m}`);
    await page.waitForSelector('main a.card.link', { timeout: 8000 }).catch(() => failed.push(`${m}: module page shows no rows`));
    for (const h of await pageLinks()) if (!links.includes(h)) links.push(h);
  }
  // tabs that no sidebar link reaches
  links.push(links[0].replace(/^(#\/\d+\/).*/, '$1raid_log'));
  links.push(links[0].replace(/^(#\/\d+\/).*/, '$1raider_matches'));
  links.push('#/commands', '#/update', '#/raiders', '#/raider-appeal');
  console.log(`${links.length} pages`);
  for (const h of links) {
    errors.length = 0;
    await page.goto(`${base}/${h}`);
    await page.waitForFunction(() => document.body.innerText.trim().length >= 40, null, { timeout: 5000 }).catch(() => {}); // the blank-page check below reports it
    await page.waitForTimeout(700); // late console errors of the page, not the render wait
    const text = (await page.innerText('body')).trim();
    const name = h.replace(/[^\w]+/g, '_');
    await page.screenshot({ path: `${out}/${name}.png` });
    // Tabs are fixed by the owner: the open tab has an orange bottom border and no filled background, on every page with tabs.
    const badTab = await page.evaluate(() => {
      const primary = getComputedStyle(document.documentElement).getPropertyValue('--primary').trim();
      // Computed lengths come back divided by the screen scale (style.css, --z); multiplied they are the CSS value.
      const z = parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--z')) || 1;
      for (const el of document.querySelectorAll('main .tabs > .on')) {
        const c = getComputedStyle(el);
        // A row with the sliding bar draws the orange line as that bar (style.css, `.tabs::after`): the bar is the 2 px orange top border.
        const bar = el.parentElement.hasAttribute('data-slide') ? getComputedStyle(el.parentElement, '::after') : null;
        const ok = bar ? parseFloat(bar.borderTopWidth) * z >= 1.99 && bar.borderTopColor !== 'rgba(0, 0, 0, 0)' : parseFloat(c.borderBottomWidth) * z >= 1.99 && c.borderBottomColor !== 'rgba(0, 0, 0, 0)';
        if (!ok || c.backgroundColor !== 'rgba(0, 0, 0, 0)') return `open tab "${el.textContent}" lost the orange underline (primary ${primary})`;
      }
      return '';
    });
    if (badTab) failed.push(`${h}: ${badTab}`);
    if (errors.length || text.length < 40) failed.push(`${h}: ${errors.join(' | ') || 'blank page'}`);
    // A ">" in an unbraced view! attribute ends the tag, and the rest of the code shows up as page text.
    if (/move \||on:\w+=/i.test(text)) failed.push(`${h}: source code shown as text`);
    // Password managers: no field may be offered to a manager (index.html quiet). A password field would be one.
    const pm = await page.evaluate(() => [...document.querySelectorAll('input,textarea')].filter((e) => e.type === 'password' || !e.hasAttribute('autocomplete') || (e.getAttribute('autocomplete') !== 'one-time-code' && !e.hasAttribute('data-1p-ignore'))).map((e) => e.outerHTML.slice(0, 80)));
    if (pm.length) failed.push(`${h}: fields open to password managers: ${pm.join(' ')}`);
  }
  // Verification: the captcha preview is drawn from the page's settings and redrawn on a change.
  await page.goto(`${base}/${links[0].replace(/^(#\/\d+\/).*/, '$1verification')}`);
  const capSrc = () => page.$eval('.capprev img', (i) => i.src);
  // With the bot's samples loaded the preview is a pressable panel: Verify shows the captcha.
  await page.locator('.capprev [data-id="verify:start"]').click({ timeout: 3000 }).catch(() => {});
  const cap1 = await page.waitForSelector('.capprev img', { timeout: 8000 }).then(capSrc).catch(() => failed.push('verification: no captcha preview'));
  await page.locator('.capprev').scrollIntoViewIfNeeded().catch(() => {});
  await page.screenshot({ path: `${out}/captcha_preview.png` });
  if (typeof cap1 === 'string') {
    await page.locator('.card:has(.capprev) button:text-is("+")').first().click();
    await page.waitForTimeout(1500);
    if ((await capSrc()) === cap1) failed.push('verification: captcha preview not redrawn after a change');
  }
  // Message Builder: a new message from the list, the bot posts it, then edits the same message.
  const embeds = links[0].replace(/^(#\/\d+\/).*/, '$1embeds');
  await page.goto(`${base}/${embeds}`);
  await page.click('button.add:has-text("Add new message")');
  await page.fill('input[aria-label="message name"]', 'e2e');
  // Typed on the message itself. Ctrl+B on a selection wraps it in Discord's ** and the preview renders it bold.
  const title = page.locator('.live .ed[aria-label="Title"]');
  await title.click();
  await page.keyboard.type('Hello');
  await page.keyboard.press('Control+a');
  await page.keyboard.press('Control+b');
  if ((await title.innerText()) !== '**Hello**') failed.push(`embeds: Ctrl+B did not bold (${await title.innerText()})`);
  await page.locator('.live .ed[aria-label="Description"]').click();
  await page.keyboard.type('Welcome to the **server**!\n> Read the *rules* first\n- be __nice__\n||secret|| and `code`');
  // Two inline fields share a row, as in Discord.
  for (const [n, v] of [['One', 'first'], ['Two', 'second']]) {
    await page.click('.live button.add:has-text("Add field")');
    await page.locator('.live .ed[aria-label="Field name"]').last().click();
    await page.keyboard.type(n);
    await page.locator('.live .ed[aria-label="Field value"]').last().click();
    await page.keyboard.type(v);
  }
  // Colour from the embed's edge, a link button, the footer time.
  await page.click('.live .embed .edge');
  await page.click('.cpop .sw10 button[title="#FF2445"]');
  await page.mouse.click(2, 2);
  await page.click('button.add:has-text("Add button row")');
  await page.click('.live .dbtn.link-b');
  await page.fill('input[aria-label="button label"]', 'Rules');
  await page.fill('input[aria-label="button link"]', 'https://example.com/rules');
  await page.click('.pop button:has-text("Done")');
  await page.click('.live .tsbtn');
  await page.click('.pop .seg button:has-text("When sent")');
  await page.mouse.click(2, 2);
  // Variables, a time tag and a server emoji go in as chips; Brim fills them in when it sends.
  const desc = page.locator('.live .ed[aria-label="Description"]');
  await desc.click();
  await page.keyboard.press('Control+End');
  await page.keyboard.type(' {');
  await page.click('.subpop .vitem:has-text("Server Name")');
  if (!(await desc.locator('.vchip[data-tok="{guild.name}"]').count())) failed.push('embeds: typing { did not add a variable');
  await desc.click();
  await page.keyboard.press('Control+End');
  await page.locator('.mcard .tools button[aria-label="Time"]').dispatchEvent('mousedown');
  await page.click('.subpop .vitem:has-text("Relative")');
  if (!(await desc.locator('.vchip.ts').count())) failed.push('embeds: Time did not add a time tag');
  await desc.click();
  await page.keyboard.press('Control+End');
  await page.locator('.mcard .tools button[aria-label="Emoji"]').dispatchEvent('mousedown');
  await page.click('.subpop .ecell[title="tb_wave"]');
  if (!(await desc.locator('img.emo').count())) failed.push('embeds: server emoji not added');
  // A select menu: one option, its private answer.
  await page.click('button.add:has-text("Add menu")');
  await page.locator('.live .ed[aria-label="Option name"]').click();
  await page.keyboard.type('Rules');
  await page.click('.dmenu .ans');
  await page.locator('.ansbox .ed').click();
  await page.keyboard.type('Thanks for picking, {');
  const mention = page.locator('.subpop .vitem:has-text("Mention")').first();
  if (await mention.isDisabled()) failed.push('embeds: member variables are off in a menu answer');
  await mention.click();
  await page.click('.pop button:has-text("Done")');
  await page.locator('.mside .mcard').screenshot({ path: `${out}/embed_editor.png` });
  await page.click('.mcard .seg button:has-text("JSON")');
  if (!(await page.locator('textarea.json').inputValue()).includes('"components"')) failed.push('embeds: JSON view has no buttons');
  await page.click('.mcard .seg button:has-text("Visual")');
  if (!(await page.locator('.dmsg .embed b', { hasText: 'Hello' }).count())) failed.push('embeds: preview did not render bold');
  if ((await page.locator('.pvwrap .embed .fields > div').count()) !== 2) failed.push('embeds: preview fields missing');
  await page.click('.pvwrap .spoiler');
  if (!(await page.locator('.pvwrap .spoiler.rev').count())) failed.push('embeds: spoiler did not open');
  // A link in the text asks before it opens; a click on any other part edits that part.
  await desc.click();
  await page.keyboard.press('Control+End');
  await page.keyboard.insertText(' [site](https://example.com/a)');
  await page.locator('.pvwrap .embed a[href]').first().click();
  if (!(await page.locator('text=Open this link?').count())) failed.push('embeds: link did not ask before opening');
  await page.keyboard.press('Escape');
  await page.locator('.pvwrap .dbtn[data-b]').first().click();
  if ((await page.locator('input[aria-label="button label"]').inputValue()) !== 'Rules') failed.push('embeds: a click on the preview button did not open its settings');
  await page.keyboard.press('Escape');
  await page.locator('.pvwrap .embed [data-p$="/title"]').first().click();
  await page.waitForFunction(() => document.activeElement?.getAttribute('data-ptr')?.endsWith('/title'), null, { timeout: 3000 }).catch(() => failed.push('embeds: a click on the preview title did not focus the title'));
  await page.click('.pvwrap .dmenu .dsel');
  await page.click('.pvwrap .dopt');
  if (!(await page.locator('.pvwrap', { hasText: 'Thanks for picking' }).locator('.eph').count())) failed.push('embeds: menu pick shows no private answer');
  if (!(await page.locator('.pvwrap', { hasText: 'My Server' }).count())) failed.push('embeds: preview shows no example for {guild.name}');
  if (!(await page.locator('.pvwrap .eph', { hasText: 'Dismiss' }).count()) || !(await page.locator('.pvwrap', { hasText: 'Thanks for picking, @Alex' }).count())) failed.push('embeds: answer has no example member');
  await page.locator('.mside .mcard').screenshot({ path: `${out}/embed_builder.png` });
  await page.click('.tabs button:has-text("Send")');
  if (!(await page.locator('.chbtn.need').count())) failed.push('embeds: no channel is not marked red');
  if (!(await page.locator('.card.send button:text-is("Send")').isDisabled())) failed.push('embeds: Send works without a channel');
  await page.click('.chbtn');
  await page.fill('input[aria-label="search channels"]', 'gen');
  await page.click('.chrow:has-text("general")');
  await page.click('.send .seg[aria-label="pings"] button:has-text("Users and roles it names")');
  await page.click('.card.send button:text-is("Send")');
  await page.waitForSelector('text=Sent. The new copy is in the Sent log.', { timeout: 5000 }).catch(() => failed.push('embeds: send failed'));
  await page.waitForSelector('.sentlog li', { timeout: 5000 }).catch(() => failed.push('embeds: Sent log is empty after a send'));
  await page.screenshot({ path: `${out}/embed_send.png` });
  await page.click('.sentlog li button:has-text("Edit message")');
  await page.waitForSelector('text=That copy now shows this version.', { timeout: 5000 }).catch(() => failed.push('embeds: edit failed'));
  // Send as a webhook: a name and an uploaded picture; the token never reaches the page.
  await page.click('.send .seg[aria-label="send as"] button:has-text("Webhook")');
  if (!(await page.locator('.card.send button:text-is("Send")').isDisabled())) failed.push('embeds: webhook without a name can be sent');
  await page.fill('input[aria-label="webhook name"]', 'News Desk');
  const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==', 'base64');
  await page.setInputFiles('input[aria-label="webhook picture file"]', { name: 'a.png', mimeType: 'image/png', buffer: png });
  await page.waitForFunction(() => /\/media\/[A-Za-z0-9_-]{43}\.png$/.test(document.querySelector('input[aria-label="webhook picture link"]')?.value || ''), null, { timeout: 5000 }).catch(() => failed.push('embeds: picture upload failed'));
  const pic = await page.inputValue('input[aria-label="webhook picture link"]');
  const got = await page.evaluate(async (u) => { const r = await fetch(new URL(u).pathname); return [r.status, r.headers.get('content-type'), r.headers.get('x-content-type-options')]; }, pic).catch(() => []);
  if (got[0] !== 200 || got[1] !== 'image/png' || got[2] !== 'nosniff') failed.push(`embeds: uploaded picture not served right: ${got}`);
  await page.click('.card.send button:text-is("Send")');
  await page.waitForSelector('text=Sent. The new copy is in the Sent log.', { timeout: 5000 }).catch(() => failed.push('embeds: webhook send failed'));
  await page.waitForSelector('.sentlog li:has-text("as the webhook")', { timeout: 5000 }).catch(() => failed.push('embeds: Sent log does not say webhook'));
  await page.screenshot({ path: `${out}/embed_webhook.png` });
  if ((await page.content()).includes('mock-webhook-token')) failed.push('embeds: webhook token reached the page');
  // A webhook deleted in Discord: Overview warns and links to the message, whose Sent log offers
  // "Send again here" (a normal send to the same channel with the same settings).
  require('child_process').execSync(`psql "${process.env.DATABASE_URL}" -qc "update message_sends set hook_gone_at = now() where webhook_id is not null"`);
  await page.goto(`${base}/${links[0].replace(/^(#\/\d+)\/.*/, '$1/overview')}`);
  const goneRow = page.locator('.chk', { hasText: 'Message Builder webhook deleted' }).first();
  await goneRow.waitFor({ timeout: 8000 }).catch(() => failed.push('embeds: Overview has no deleted-webhook warning'));
  if (!(await goneRow.count()) || !(await goneRow.innerText()).includes('Send again here')) failed.push('embeds: deleted-webhook warning does not say how to resend');
  await page.screenshot({ path: `${out}/overview_webhook_gone.png` });
  await goneRow.locator('a:text-is("Open page")').click().catch(() => failed.push('embeds: deleted-webhook warning has no link'));
  await page.waitForSelector('.sentlog button:text-is("Send again here")', { timeout: 8000 }).catch(() => failed.push('embeds: link does not open the Sent log with Send again here'));
  await page.screenshot({ path: `${out}/embed_send_again.png` });
  if (!(await page.locator('.sentlog li', { hasText: 'can no longer be edited' }).locator('button:text-is("Edit message")').isDisabled())) failed.push('embeds: Edit message stays on for a copy whose webhook is gone');
  await page.click('.sentlog button:text-is("Send again here")');
  await page.waitForSelector('text=Sent. The new copy is in the Sent log.', { timeout: 5000 }).catch(() => failed.push('embeds: Send again here failed'));
  const left = require('child_process').execSync(`psql "${process.env.DATABASE_URL}" -qAtc "select count(*) from message_sends s where hook_gone_at is null and webhook_id is not null"`).toString().trim();
  if (left !== '1') failed.push(`embeds: Send again here made ${left} new webhook copies, not 1`);
  // Sent log delete: the copy whose webhook is gone is deleted (as the bot) after a confirm, and its row goes.
  const rowsBefore = await page.locator('.sentlog li').count();
  await page.locator('.sentlog li', { hasText: 'can no longer be edited' }).first().locator('button[aria-label="delete this copy"]').click();
  await page.screenshot({ path: `${out}/sent_delete_confirm.png` });
  await page.click('.modal button:text-is("Delete")');
  await page.waitForSelector('text=Copy deleted in Discord and removed from the Sent log.', { timeout: 8000 }).catch(() => failed.push('embeds: Sent log delete failed'));
  await page.waitForFunction((n) => document.querySelectorAll('.sentlog li').length === n - 1, rowsBefore, { timeout: 5000 }).catch(() => failed.push('embeds: deleted row still in the Sent log'));
  await page.screenshot({ path: `${out}/sent_delete_done.png` });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: `${out}/sent_delete_done_phone.png`, fullPage: true });
  await page.setViewportSize({ width: 1280, height: 900 });
  // Schedules: a weekly time, shown with its next runs, saved, then run once by the dashboard.
  await page.click('.tabs button:has-text("Automate")');
  await page.click('button.add:has-text("Add new schedule")');
  if (!(await page.locator('.rule button:text-is("Save schedule")').isDisabled())) failed.push('schedules: a schedule without a channel can be saved');
  await page.click('.rule .chbtn');
  await page.fill('input[aria-label="search channels"]', 'gen');
  await page.click('.chrow:has-text("general")');
  await page.fill('.rule input[aria-label="time zone"]', 'Europe/Oslo');
  await page.dispatchEvent('.rule input[aria-label="time zone"]', 'change');
  await page.selectOption('.rule select[aria-label="kind of time"]', 'weekly');
  await page.click('.rule .chipset button:text-is("Mon")');
  await page.click('.rule .chipset button:text-is("Fri")');
  await page.waitForSelector('.rule .nexts:has-text("Mon")', { timeout: 5000 }).catch(() => failed.push('schedules: no next runs shown'));
  await page.screenshot({ path: `${out}/schedule_edit.png` });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: `${out}/schedule_edit_phone.png`, fullPage: true });
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.click('.rule button:text-is("Save schedule")');
  await page.waitForSelector('text=Schedule saved.', { timeout: 5000 }).catch(() => failed.push('schedules: save failed'));
  await page.waitForSelector('.rcard:has-text("Mon, Fri")', { timeout: 5000 }).catch(() => failed.push('schedules: saved schedule not listed'));
  await page.screenshot({ path: `${out}/schedule_list.png` });
  const psql = (q) => require('child_process').execSync(`psql "${process.env.DATABASE_URL}" -qAtc "${q}"`).toString().trim();
  // A run that is due now: a quarter-hour repeat that started 15 minutes before this minute.
  const minute = Math.floor(Date.now() / 60000) * 60;
  psql(`update message_rules set spec = jsonb_set(spec, '{lines}', '[{\\"kind\\": \\"every\\", \\"minutes\\": 15}]'::jsonb) || '{\\"start\\": ${minute - 900}}'::jsonb, next_at = to_timestamp(${minute})`);
  let ran = '0';
  for (let i = 0; i < 30 && ran === '0'; i++) { await page.waitForTimeout(1000); ran = psql("select count(*) from message_sends where source = 'schedule'"); }
  if (ran !== '1') failed.push(`schedules: a due schedule sent ${ran} copies, not 1`);
  if (psql("select count(*) from message_rules where next_at > now() + interval '10 minutes'") !== '1') failed.push('schedules: next run not moved after a run');
  await page.click('.rcard button[aria-label="delete schedule"]');
  await page.click('.modal button:text-is("Delete")');
  await page.waitForSelector('text=Schedule deleted.', { timeout: 5000 }).catch(() => failed.push('schedules: delete failed'));
  // Triggers: a word in #general, tried, saved; a queued answer goes out as a reply that never
  // pings, then deletes itself.
  await page.click('button.add:has-text("Add new trigger")');
  if (!(await page.locator('.trig button:text-is("Save trigger")').isDisabled())) failed.push('triggers: a trigger without a channel can be saved');
  await page.click('.trig .chbtn');
  await page.fill('input[aria-label="search channels"]', 'gen');
  await page.click('.chrow:has-text("general")');
  await page.fill('.trig input[aria-label="word, phrase or pattern"]', 'hello');
  await page.click('.trig button.add:has-text("Add word")');
  await page.fill('.trig input[aria-label="try a message"]', 'well Hello there');
  await page.waitForSelector('.trig p:has-text("Matches “hello”")', { timeout: 5000 }).catch(() => failed.push('triggers: Try a message did not match'));
  await page.fill('.trig input[aria-label="try a message"]', 'othello');
  await page.waitForSelector('.trig p:has-text("No trigger matches")', { timeout: 5000 }).catch(() => failed.push('triggers: whole words did not hold'));
  await page.screenshot({ path: `${out}/trigger_edit.png` });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: `${out}/trigger_edit_phone.png`, fullPage: true });
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.click('.trig button:text-is("Save trigger")');
  await page.waitForSelector('text=Trigger saved.', { timeout: 5000 }).catch(() => failed.push('triggers: save failed'));
  await page.waitForSelector('.rcard:has-text("“hello” in #general")', { timeout: 5000 }).catch(() => failed.push('triggers: saved trigger not listed'));
  // This message sends as a webhook, which cannot reply: answer as Brim to see the reply.
  psql("update embeds set webhook = null where webhook is not null");
  psql("insert into message_runs (rule_id, guild_id, channel_id, member_id, message_id, matched) select id, guild_id, 300000000000000001, 200000000000000004, 777, 'hello' from message_rules where kind = 'trigger'; notify message_runs");
  let answered = '0';
  for (let i = 0; i < 20 && answered === '0'; i++) { await page.waitForTimeout(500); answered = psql("select count(*) from message_sends where source = 'trigger'"); }
  if (answered !== '1') failed.push(`triggers: a queued answer sent ${answered} copies, not 1`);
  const replies = (await (await fetch(`${mock}/sent`)).json()).filter((m) => m.body && m.body.includes('"message_reference"'));
  if (!replies.length || !replies[0].body.includes('"message_id":"777"') || !replies[0].body.includes('"allowed_mentions":{"parse":[]}')) failed.push(`triggers: answer is not a reply without pings: ${replies[0] && replies[0].body}`);
  psql("update message_sends set delete_at = now() where source = 'trigger'");
  let deleted = false;
  for (let i = 0; i < 30 && !deleted; i++) { await page.waitForTimeout(500); deleted = psql("select count(*) from message_sends where source = 'trigger' and posted_id is null") === '1'; }
  if (!deleted) failed.push('triggers: delete after did not delete the answer');
  await page.screenshot({ path: `${out}/trigger_list.png` });
  await page.click('.rcard button[aria-label="delete trigger"]');
  await page.click('.modal button:text-is("Delete")');
  await page.waitForSelector('text=Trigger deleted.', { timeout: 5000 }).catch(() => failed.push('triggers: delete failed'));
  // Sticky: made in #general and posted at once; a repost posts a new copy, then deletes the old one.
  await page.click('.tabs button:has-text("Automate")');
  await page.click('button.add:has-text("Add channel")');
  if (!(await page.locator('button:text-is("Save sticky")').isDisabled())) failed.push('sticky: a sticky without a channel can be saved');
  await page.click('.rule .chbtn');
  await page.fill('input[aria-label="search channels"]', 'gen');
  await page.click('.chrow:has-text("general")');
  await page.screenshot({ path: `${out}/sticky_edit.png` });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: `${out}/sticky_edit_phone.png`, fullPage: true });
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.click('button:text-is("Save sticky")');
  await page.waitForSelector('text=Sticky saved.', { timeout: 5000 }).catch(() => failed.push('sticky: save failed'));
  await page.waitForSelector('.rcard:has-text("#general")', { timeout: 5000 }).catch(() => failed.push('sticky: saved sticky not listed'));
  let stuck = '0';
  for (let i = 0; i < 20 && stuck === '0'; i++) { await page.waitForTimeout(500); stuck = psql("select count(*) from sticky_msgs where posted_id is not null"); }
  if (stuck !== '1') failed.push('sticky: not posted when saved');
  // The mock gives every message id 1: give the first copy its own id to see it deleted.
  psql("update message_sends set posted_id = 5 where source = 'sticky'; update sticky_msgs set posted_id = 5");
  psql("insert into sticky_runs (channel_id, guild_id, bots_only) select channel_id, guild_id, false from sticky_msgs; notify message_runs");
  let moved = false;
  for (let i = 0; i < 20 && !moved; i++) { await page.waitForTimeout(500); moved = psql("select count(*) from sticky_msgs where posted_id = 1") === '1'; }
  if (!moved) failed.push('sticky: a queued repost did not post a new copy');
  const gone = (await (await fetch(`${mock}/sent`)).json()).some((m) => m.method === 'DELETE' && m.path === '/channels/300000000000000001/messages/5');
  if (!gone) failed.push('sticky: the old copy was not deleted');
  if (psql("select count(*) from message_sends where source = 'sticky' and posted_id is not null") !== '1') failed.push('sticky: more than one copy is up');
  await page.screenshot({ path: `${out}/sticky_list.png` });
  // Its webhook deleted in Discord: the card says so (it clears at the next repost).
  psql("update message_sends set hook_gone_at = now() from sticky_msgs k where message_sends.id = k.send_id");
  await page.click('.tabs button:has-text("Send")');
  await page.click('.tabs button:has-text("Automate")');
  await page.waitForSelector('.rcard .err:has-text("webhook for this copy was deleted")', { timeout: 5000 }).catch(() => failed.push('sticky: no notice for a deleted webhook'));
  await page.screenshot({ path: `${out}/sticky_hook_gone.png` });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: `${out}/sticky_hook_gone_phone.png`, fullPage: true });
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.click('.rcard button[aria-label="remove sticky"]');
  await page.click('.modal button:text-is("Remove")');
  await page.waitForSelector('text=Sticky removed.', { timeout: 5000 }).catch(() => failed.push('sticky: remove failed'));
  // Tag: set in the Automate tab; a queued /tag run posts as Brim with no pings and writes its result.
  await page.fill('#mbtag', 'Rules');
  await page.click('button:text-is("Save tag")');
  await page.waitForSelector('text=Tag saved.', { timeout: 5000 }).catch(() => failed.push('tag: save failed'));
  if (psql("select count(*) from embeds where tag = 'rules'") !== '1') failed.push('tag: not stored lower case');
  psql("insert into message_tag_runs (guild_id, embed_id, channel_id, member_id, member_name) select guild_id, id, 300000000000000001, 200000000000000007, 'e2e-mod' from embeds where tag = 'rules'; notify message_runs");
  let tagged = '';
  for (let i = 0; i < 20 && !tagged; i++) { await page.waitForTimeout(500); tagged = psql("select coalesce(result, '') from message_tag_runs"); }
  if (!tagged.startsWith('Posted')) failed.push(`tag: run result ${tagged}`);
  if (psql("select count(*) from message_sends where detail = 'Sent by e2e-mod with /tag rules'") !== '1') failed.push('tag: no Sent log entry');
  // Built-in tag: a run with an entry key (no saved message) posts that entry.
  psql("insert into message_tag_runs (guild_id, entry, channel_id, member_id, member_name) select guild_id, 'tag.dangerous_permissions', 300000000000000001, 200000000000000008, 'e2e-mod' from embeds where tag = 'rules'; notify message_runs");
  let builtIn = '';
  for (let i = 0; i < 20 && !builtIn; i++) { await page.waitForTimeout(500); builtIn = psql("select coalesce(result, '') from message_tag_runs where entry is not null"); }
  if (builtIn !== 'Posted the built-in tag.') failed.push(`tag: built-in run result ${builtIn}`);
  await page.screenshot({ path: `${out}/tag_card.png` });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: `${out}/tag_card_phone.png`, fullPage: true });
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.click('button:text-is("Remove tag")');
  await page.waitForSelector('text=Tag removed.', { timeout: 5000 }).catch(() => failed.push('tag: remove failed'));
  await page.click('text=← Saved messages');
  if (!(await page.locator('.mrow', { hasText: 'e2e' }).locator('.states .tip').count())) failed.push('embeds: list shows no Sent icon');
  const sent = await (await fetch(`${mock}/sent`)).json();
  if (!sent.some((m) => /\/messages\/1$/.test(m.path) && m.body.includes('Hello'))) failed.push('embeds: no edit reached Discord');
  const post = sent.find((m) => /\/messages$/.test(m.path) && m.body.includes('Hello'));
  const hooked = sent.find((m) => /^\/webhooks\/500000000000000001\/mock-webhook-token\?wait=true&with_components=true$/.test(m.path));
  if (!hooked || !hooked.body.includes('"username":"News Desk"') || !/"avatar_url":"http[^"]*\/media\/[A-Za-z0-9_-]{43}\.png"/.test(hooked.body)) failed.push(`embeds: webhook send is wrong: ${hooked && hooked.body}`);
  if (!post || !post.body.includes('"allowed_mentions":{"parse":[],"roles":[],"users":[]}') && !post.body.includes('"allowed_mentions":{"parse":[],"users":[],"roles":[]}')) failed.push(`embeds: pings setting not sent: ${post && post.body}`);
  if (!post || !post.body.includes('example.com/rules') || /"timestamp":"now"/.test(post.body)) failed.push(`embeds: posted body is wrong: ${post && post.body}`);
  if (!post || !post.body.includes('"custom_id":"mb:') || post.body.includes('answer') || post.body.includes('{guild.name}') || !/<t:\d+:R>/.test(post.body) || !post.body.includes('<:tb_wave:400000000000000001>')) failed.push(`embeds: menu, variable, time or emoji not sent right: ${post && post.body}`);
  // Notes: an edit keeps the created time and shows up in the note's history.
  await page.goto(`${base}/${links[0].replace(/^(#\/\d+\/).*/, '$1notes')}`);
  // The page rebuilds its form when its data arrives; a fill before that is lost and Add note answers 400.
  await page.waitForLoadState('networkidle');
  await page.fill('input[aria-label="user ID"]', '42');
  const tag = Date.now();
  await page.fill('input[aria-label="new note"]', `first note ${tag}`);
  await page.click('text=Add note');
  // Newest note first; earlier runs may have left notes behind.
  // Wait for this run's note: older notes are listed at once, and the reload that adds the new one on top would swap the card under the fill.
  await page.waitForFunction((t) => document.querySelector('input[aria-label="note text"]')?.value === t, `first note ${tag}`, { timeout: 5000 }).catch(() => failed.push('notes: added note not listed'));
  const card = page.locator('.card:has(input[aria-label="note text"])').first();
  await card.locator('input[aria-label="note text"]').fill(`edited note ${tag}`);
  await card.locator('button', { hasText: 'Save' }).click();
  await card.locator('summary', { hasText: 'History (1)' }).click({ timeout: 5000 }).catch(() => failed.push('notes: no history after edit'));
  if (!(await card.locator('li', { hasText: `first note ${tag} → edited note ${tag}` }).count())) failed.push('notes: history misses the change');
  await page.screenshot({ path: `${out}/notes_history.png` });
  // Log setup reuses the public "Logs" category and makes it private (hides it from @everyone).
  const gid = links[0].match(/^#\/(\d+)\//)[1];
  // Setup is a sensitive action; mark the session as stepped up instead of driving TOTP.
  require('child_process').execSync(`psql "${process.env.DATABASE_URL}" -qc "update dash_session set step_up_until = now() + interval '1 hour', step_up_guild = ${gid}"`);
  // The sidebar module switch: turning off asks first, turning on does not; both save through the section's PUT.
  await page.goto(`${base}/#/${gid}/module/anti_nuke`);
  const SW = 'nav input.sw[aria-label="Anti Nuke on or off"]';
  // Every module shows its master switch, open or not.
  await page.waitForSelector(SW, { timeout: 8000 }).catch(() => {});
  const navSw = await page.$$eval('nav input.sw', (l) => l.map((e) => e.getAttribute('aria-label')).sort().join());
  if (navSw !== 'Anti Nuke on or off,Auto Mod on or off,Logging on or off,Messages on or off,Moderation on or off,Server Joins on or off,Tickets on or off') failed.push(`module switch: sidebar shows ${navSw}`);
  // A part with its own on/off shows its switch on the module page.
  await page.goto(`${base}/#/${gid}/module/server_joins`);
  const partSw = await page.waitForFunction(() => document.querySelectorAll('main a.card.link input.sw').length === 4, null, { timeout: 8000 }).then(() => true).catch(() => false);
  if (!partSw) failed.push('part switch: Server Joins does not show 4 part switches');
  await page.goto(`${base}/#/${gid}/module/anti_nuke`);
  const sw = page.locator(SW);
  await sw.waitFor({ timeout: 8000 }).catch(() => failed.push('module switch: none in the sidebar'));
  const swWas = await sw.isChecked().catch(() => null);
  if (swWas !== null) {
    if (!swWas) await sw.click(); // make sure it starts On
    await page.waitForFunction((SW) => document.querySelector(SW)?.checked, SW, { timeout: 8000 }).catch(() => failed.push('module switch: did not turn on'));
    await sw.click();
    if (!(await page.locator('.modal').waitFor({ timeout: 3000 }).then(() => true).catch(() => false))) failed.push('module switch: no confirm before turning off');
    await page.locator('.modal button.danger').click();
    await page.waitForFunction((SW) => document.querySelector(SW) && !document.querySelector(SW).checked, SW, { timeout: 8000 }).catch(() => failed.push('module switch: did not turn off'));
    await sw.click(); // On again: no dialog
    await page.waitForFunction((SW) => document.querySelector(SW)?.checked, SW, { timeout: 8000 }).catch(() => failed.push('module switch: did not turn back on'));
    if (await page.locator('.modal').count()) failed.push('module switch: asked before turning on');
    if (!swWas) { await sw.click(); await page.locator('.modal button.danger').click(); }
  }
  const setup = await page.evaluate(async (g) => {
    const csrf = (await (await fetch('/api/me')).json()).csrf;
    const { version } = await (await fetch(`/api/guilds/${g}/config`)).json();
    const r = await fetch(`/api/guilds/${g}/logging/setup`, {
      method: 'POST', headers: { 'content-type': 'application/json', 'x-csrf-token': csrf },
      body: JSON.stringify({ expected_version: version }),
    });
    return `${r.status} ${await r.text()}`;
  }, gid);
  const after = await (await fetch(`${mock}/sent`)).json();
  if (!after.some((m) => m.path === `/channels/300000000000000002/permissions/${gid}`)) failed.push(`log setup: public Logs category not made private (${setup})`);
  // Permits: a sortable list and one add button, also with no permits of your own. Extra Owner and Trusted Admin are
  // fixed rows with a lock; a permit row moves with the arrow keys; Delete is in Edit, not on the row.
  await page.goto(`${base}/#/${gid}/permits`);
  await page.waitForSelector('.card.slist', { timeout: 8000 }).catch(() => failed.push('permits: no sortable list'));
  const permitAdds = await page.locator('button.add:has-text("Add new permit")').count();
  if (permitAdds !== 1) failed.push(`permits: ${permitAdds} Add new permit buttons, want 1`);
  if ((await page.locator('.slist .srow.fixed .grip').count()) !== 2) failed.push('permits: Extra Owner / Trusted Admin rows have no lock');
  for (const nm of ['P1', 'P2']) {
    await page.click('button.add:has-text("Add new permit")');
    await page.fill('.modal input[aria-label="permit name"]', nm);
    await page.press('.modal input[aria-label="permit name"]', 'Tab');
    if (nm === 'P1' && !(await page.locator('.modal button.danger:has-text("Delete")').count())) failed.push('permits: no Delete in the Edit dialog');
    await page.click('.modal button:has-text("Apply")');
    await page.waitForSelector('.modal', { state: 'detached' }).catch(() => {});
  }
  const rowNames = () => page.$$eval('.slist .srow:not(.fixed) .sname b', (b) => b.map((x) => x.textContent));
  await page.focus('.slist .srow[data-i="0"]');
  await page.keyboard.press('ArrowDown');
  await page.waitForTimeout(300);
  const permitOrder = await rowNames();
  if (permitOrder.join() !== 'P2,P1') failed.push(`permits: ArrowDown did not move the row (${permitOrder})`);
  const focused = await page.evaluate(() => document.activeElement?.getAttribute('aria-label'));
  if (focused !== 'P1') failed.push(`permits: focus left the moved row (${focused})`);
  const rowBtns = await page.$$eval('.slist .srow button', (b) => b.map((x) => x.textContent.trim()).filter((t) => t !== 'Edit'));
  if (rowBtns.length) failed.push(`permits: row buttons other than Edit: ${rowBtns}`);
  // A mouse drags the whole row (native drag); Delete is in Edit and asks first.
  await page.dragAndDrop('.slist .srow[data-i="1"]', '.slist .srow[data-i="0"]');
  await page.waitForTimeout(300);
  if ((await rowNames()).join() !== 'P1,P2') failed.push(`permits: drag did not move the row (${await rowNames()})`);
  await page.click('.slist .srow[data-i="0"] button:has-text("Edit")');
  await page.click('.modal button.danger:has-text("Delete")');
  await page.click('.modal[aria-label="Delete permit"] button.danger:has-text("Delete")');
  await page.waitForSelector('.modal', { state: 'detached' }).catch(() => failed.push('permits: a dialog stayed open after Delete'));
  if ((await rowNames()).join() !== 'P2') failed.push(`permits: Delete in Edit did not remove the permit (${await rowNames()})`);
  await page.click('.bar button:has-text("Cancel")');
  // Backups (layout PR 6): no browser frame around the settings; a backup's own page is a route with a crumb
  // ("Backups › Backup #code"), no "Go back" button. An unknown code shows the list and says so.
  await page.goto(`${base}/#/${gid}/backups`);
  await page.waitForSelector('#grp-general', { timeout: 8000 }).catch(() => failed.push('backups: no settings cards'));
  const frame = await page.evaluate(() => [...document.querySelectorAll('main fieldset')].map((f) => getComputedStyle(f).borderTopStyle).filter((b) => b !== 'none'));
  if (frame.length) failed.push(`backups: fieldset frame ${frame}`);
  await page.goto(`${base}/#/${gid}/backups/zzzzzz`);
  await page.waitForSelector('text=There is no backup #zzzzzz', { timeout: 8000 }).catch(() => failed.push('backups: unknown code not reported'));
  if (!(await page.locator('.head small a:has-text("Backups")').count())) failed.push('backups: detail crumb has no link back to Backups');
  if (await page.locator('main button:has-text("Go back")').count()) failed.push('backups: a Go back button is left');
  if (!(await page.locator('button:has-text("Create backup")').isVisible())) failed.push('backups: list hidden after an unknown code');
  // Update page: the mock user owns the app; no updater runs here, so the page says how to start it.
  await page.goto(`${base}/#/update`);
  await page.waitForSelector('text=The updater is not running', { timeout: 5000 }).catch(() => failed.push('update: no updater notice'));
  if (!(await page.locator('header a[href="#/update"]').count())) failed.push('update: no header link for the bot owner');
  // An update found: each merged pull request's line shows under Newest, as plain text (GitHub is not reachable here).
  const a = 'a'.repeat(40), b = 'b'.repeat(40);
  await page.route('**/api/update', (r) => r.fulfill({ json: { current: a, latest: { sha: b, title: 'Merge pull request #244 from x/y' }, newer: true, updater: true, run: null, estimate: [2, 60, 10, 10, 20],
    changes: [{ pr: 244, what: 'Message Builder phase 8: drop the old embed column' }, { pr: 243, what: '<b>Setup wizard</b> redesign' }, { pr: 242, what: '' }], more: 2,
    builds: [{ sha: b, version: 'v1.1.0', at: '2026-11-21T18:00:00Z' }, { sha: a, version: 'v1.0.0', at: '2026-11-20T18:00:00Z' }] } }));
  await page.reload();
  // Releases show by version: Running is v1.0.0, Newest v1.1.0.
  await page.waitForSelector('p:has-text("Newest: v1.1.0")', { timeout: 5000 }).catch(() => failed.push('update: Newest does not name the release'));
  if (!(await page.locator('p', { hasText: 'Running: v1.0.0' }).count())) failed.push('update: Running does not name the release');
  await page.waitForSelector('li:has-text("Message Builder phase 8")', { timeout: 5000 }).catch(() => failed.push('update: no change lines'));
  if (!(await page.locator('li', { hasText: '<b>Setup wizard</b>' }).count())) failed.push('update: a change line is not shown as plain text');
  if (!(await page.locator('li', { hasText: 'and 2 more' }).count())) failed.push('update: no "and N more"');
  await page.screenshot({ path: `${out}/update.png` });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: `${out}/update_phone.png`, fullPage: true });
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.unroute('**/api/update');
  // Channel Setup tabs switch: the Channels grid is gone under Roles and Server, and back again.
  await page.goto(`${base}/${links[0].replace(/^(#\/\d+\/).*/, '$1channel_setup')}`);
  await page.waitForSelector('.cs-tabs button', { timeout: 5000 }).catch(() => failed.push('channel setup: no tabs'));
  for (const t of ['Roles', 'Server']) {
    await page.locator('.cs-tabs button', { hasText: t }).click();
    if (await page.locator('.cs-grid:has(select.addch)').isVisible()) failed.push(`channel setup: Channels still shows under ${t}`);
  }
  await page.locator('.cs-tabs button', { hasText: 'Channels' }).click();
  if (!(await page.locator('.cs-grid:has(select.addch)').isVisible())) failed.push('channel setup: Channels did not come back');
  // Number fields: a ">" inside a view! attribute once ended the tag and printed the code as text.
  await page.goto(`${base}/${links[0].replace(/^(#\/\d+\/).*/, '$1automod')}`);
  await page.waitForSelector('.step input', { timeout: 5000 }).catch(() => failed.push('automod: no number field'));
  if (/num\(\)/i.test(await page.innerText('body'))) failed.push('automod: number field shows source code');
  // Tickets: the button card shows a preview, the panel editor adds a panel, and the Publish, list and stats cards show.
  await page.goto(`${base}/${links[0].replace(/^(#\/\d+\/).*/, '$1tickets')}`);
  // Waits: count right after goto ran before the page had loaded.
  await page.locator('h3', { hasText: 'The ticket button' }).waitFor({ timeout: 5000 }).catch(() => failed.push('tickets: no ticket button card'));
  await page.locator('.dmsg', { hasText: 'Open a ticket' }).waitFor({ timeout: 5000 }).catch(() => failed.push('tickets: no ticket button preview'));
  await page.click('summary:text-is("More buttons and forms")', { timeout: 5000 }).catch(() => failed.push('tickets: no More buttons and forms section'));
  await page.click('button.add:has-text("Add panel")', { timeout: 5000 }).catch(() => failed.push('tickets: no Add panel button'));
  // The first panel Add makes is the #open-a-ticket button: a label members understand, not "New panel".
  if (!(await page.locator('details summary', { hasText: 'Create ticket' }).count())) failed.push('tickets: Add panel made no panel');
  // The message keeps the look of the ticket.panel entry (Default messages), so it is never a bare title.
  if (!(await page.locator('.dmsg', { hasText: 'Need a hand from the staff' }).count())) failed.push('tickets: Add panel left the message without text');
  if (!(await page.locator('a', { hasText: 'Edit message' }).count())) failed.push('tickets: no Edit message link to Default messages');
  for (const t of ['Publish panels', 'Last 30 days']) if (!(await page.locator('h2', { hasText: t }).count())) failed.push(`tickets: no ${t} card`);
  await page.click('.bar >> text=Cancel', { timeout: 5000 }).catch(() => failed.push('tickets: no Changes detected bar after Add panel'));
  // Publish panels: the channel box opens the list of channels on click and filters as you type (the shared picker).
  await page.goto(`${base}/${links[0].replace(/^(#\/\d+\/).*/, '$1tickets/publish')}`);
  const pub = page.locator('.card', { has: page.locator('h2', { hasText: 'Publish panels' }) }).locator('.lp input');
  await pub.click();
  await page.locator('.lpl li[role=option]').nth(1).waitFor({ timeout: 5000 }).catch(() => failed.push('tickets: Publish panels channel box lists no channels'));
  const all = await page.locator('.lpl li[role=option]').count();
  await pub.fill('zzzz-no-such');
  if (!(await page.locator('.lpl li', { hasText: 'No match' }).count())) failed.push('tickets: Publish panels channel box does not filter');
  await pub.fill('');
  if ((await page.locator('.lpl li[role=option]').count()) !== all) failed.push('tickets: Publish panels channel box lost its list after clearing the search');
  await page.keyboard.press('Escape');
  // A picker in a dialog: the open list is drawn on top of the dialog, fully inside the window, never cut at the dialog's edge.
  await page.goto(`${base}/${links[0].replace(/^(#\/\d+\/).*/, '$1feeds')}`);
  await page.click('button:has-text("Add new feed")', { timeout: 5000 }).catch(() => failed.push('picker in dialog: no Add new feed button'));
  for (const [w, h] of [[1400, 900], [390, 500]]) {
    await page.setViewportSize({ width: w, height: h });
    await page.locator('.modal .lp input').first().click();
    await page.locator('.lpl li[role=option]').first().waitFor({ timeout: 5000 }).catch(() => failed.push(`picker in dialog: no list at ${w}`));
    await page.waitForTimeout(150);
    const r = await page.evaluate(() => {
      const l = document.querySelector('.lpl'); if (!l) return null;
      const b = l.getBoundingClientRect(), f = document.querySelector('.modal .lp input').getBoundingClientRect();
      const top = document.elementFromPoint(b.left + b.width / 2, b.top + Math.min(b.height / 2, 20));
      return { inside: b.top >= 0 && b.bottom <= innerHeight && b.left >= 0 && b.right <= innerWidth, onTop: !!top && !!top.closest('.lpl'), gap: Math.min(Math.abs(b.top - f.bottom), Math.abs(f.top - b.bottom)) };
    });
    if (!r || !r.inside || !r.onTop || r.gap > 12) failed.push(`picker in dialog: list not fully visible, on top and attached at ${w}x${h} (${JSON.stringify(r)})`);
    await page.keyboard.press('Escape');
  }
  await page.setViewportSize({ width: 1400, height: 900 });
  // Bot messages: the grouped list, a message opens in the editor, Revert asks first.
  await page.goto(`${base}/${links[0].replace(/^(#\/\d+\/).*/, '$1bot_messages')}`);
  // Six main areas, all closed at first; an area opens to its sub-groups, a sub-group to its messages.
  await page.locator('button.card.hub', { hasText: 'Moderation' }).first().waitFor({ timeout: 5000 }).catch(() => failed.push('bot messages: no Moderation area'));
  if ((await page.locator('button.card.hub:not(.sub)').count()) !== 6) failed.push('bot messages: not 6 main areas');
  if (await page.locator('.mrow').count()) failed.push('bot messages: an area is open at first');
  // Nothing on the page starts left of the window or makes it scroll sideways (a cut-off left edge),
  // closed and with an area and a sub-group open. Hidden tips (opacity 0) do not count.
  const noCut = async (when) => {
    for (const w of [390, 1400]) {
      await page.setViewportSize({ width: w, height: 900 });
      const cut = await page.evaluate(() => [...document.querySelectorAll('main *')].filter((e) => { const r = e.getBoundingClientRect(); return r.width > 0 && e.checkVisibility({ opacityProperty: true, visibilityProperty: true }) && (r.left < -0.5 || r.right > innerWidth + 0.5); }).length + (document.documentElement.scrollWidth > innerWidth ? 1 : 0));
      if (cut) failed.push(`bot messages: ${cut} parts cut off at ${w} px (${when})`);
    }
    await page.setViewportSize({ width: 1400, height: 900 });
  };
  await noCut('closed');
  // The recovery texts are never listed (nobody may change them).
  for (const t of ['Dashboard 2FA setup code', 'Rescue key expired']) { await page.locator('input[aria-label="search messages"]').fill(t); if (await page.locator('.mrow', { hasText: t }).count()) failed.push(`bot messages: ${t} is listed`); }
  await page.locator('input[aria-label="search messages"]').fill('');
  await page.click('button.card.hub:not(.sub):has-text("Security")');
  await page.click('button.card.hub.sub:has-text("Anti-nuke")');
  if (!(await page.locator('.chip', { hasText: 'Security' }).count())) failed.push('bot messages: no security text chip');
  await noCut('Security open');
  await page.click('button.card.hub:not(.sub):has-text("Security")');
  await page.click('button.card.hub:not(.sub):has-text("Moderation")');
  await page.click('button.card.hub.sub:has-text("Punishment DMs")');
  await page.click('.mrow .nm b:text-is("Ban")', { timeout: 5000 }).catch(() => failed.push('bot messages: no Ban row'));
  // The editor is open on the left and the preview on the right.
  await page.locator('.msplit .mview .dmsg').first().waitFor({ timeout: 5000 }).catch(() => failed.push('bot messages: no preview beside the message'));
  await page.locator('.ed[aria-label="Title"]').first().waitFor({ timeout: 5000 }).catch(() => failed.push('bot messages: Ban did not open in the editor'));
  if (await page.locator('button', { hasText: 'Add button row' }).count()) failed.push('bot messages: the editor offers buttons');
  await page.click('button:text-is("← Default messages")', { timeout: 5000 }).catch(() => failed.push('bot messages: no way back'));
  // Buttons Brim adds: drawn once with a notice, not pressable, and the preview follows what is typed.
  const search = page.locator('input[aria-label="search messages"]');
  await search.fill('Giveaway message (running)');
  await page.click('.mrow .nm b:text-is("Giveaway message (running)")', { timeout: 5000 }).catch(() => failed.push('bot messages: no running giveaway row'));
  await page.locator('.mview .dbtn.added', { hasText: 'Enter' }).waitFor({ timeout: 5000 }).catch(() => failed.push('bot messages: the Enter button is not drawn in the preview'));
  if ((await page.locator('.added-note').count()) !== 1) failed.push('bot messages: the notice for added buttons is not shown once');
  const here = page.url();
  await page.locator('.mview .dbtn.added').first().click();
  if (page.url() !== here || (await page.locator('[role="dialog"]').count())) failed.push('bot messages: pressing the drawn Enter button did something');
  await page.locator('.ed[aria-label="Description"]').first().click({ position: { x: 5, y: 5 } });
  await page.keyboard.press('Escape');
  await page.keyboard.type(' ZZtyped');
  if (!(await page.locator('.mview .embed', { hasText: 'ZZtyped' }).count())) failed.push('bot messages: the preview does not follow typing');
  if ((await page.locator('.added-note').count()) !== 1) failed.push('bot messages: the notice shows twice while editing');
  await page.click('button:text-is("← Default messages")', { timeout: 5000 }).catch(() => failed.push('bot messages: no way back from the running giveaway'));
  await page.locator('[role="dialog"] button:text-is("Discard changes")').click({ timeout: 1000 }).catch(() => {});
  // Search finds a message by name or text; a group folds shut and a search opens it again.
  const find = page.locator('input[aria-label="search messages"]');
  await find.fill('zzqqxx');
  await page.locator('text=No messages found.').waitFor({ timeout: 5000 }).catch(() => failed.push('bot messages: no empty state for a search with no hits'));
  await find.fill('ban');
  if (!(await page.locator('.mrow .nm b:text-is("Ban")').count())) failed.push('bot messages: search does not find Ban');
  await find.fill('');
  await page.click('button.card.hub.sub:has-text("Punishment DMs")');
  if (await page.locator('.mrow .nm b:text-is("Ban")').count()) failed.push('bot messages: a folded group still shows its messages');
  await find.fill('ban');
  if (!(await page.locator('.mrow .nm b:text-is("Ban")').count())) failed.push('bot messages: a search does not open a folded group');
  await find.fill('');
  // An entry with {text} says it must keep it; removing it turns the notice red and offers Insert Bot text.
  await find.fill('Message Edited');
  await page.click('.mrow .nm b:text-is("Message Edited")', { timeout: 5000 }).catch(() => failed.push('bot messages: no Message Edited row'));
  await page.locator('.note.req-text').waitFor({ timeout: 5000 }).catch(() => failed.push('bot messages: no Bot text notice on a log entry'));
  if (await page.locator('.note.req-text.bad').count()) failed.push('bot messages: the default log entry is marked as missing {text}');
  await page.click('button:text-is("← Default messages")', { timeout: 5000 }).catch(() => failed.push('bot messages: no way back from a log entry'));
  await find.fill('');
  // Cancel on a changed setting puts dropdowns back too: a plain `selected=` attribute stops counting once the user has picked.
  // Cases (review): a row opens the case page; an edit, a new proof and a removed proof show there.
  const cg = links[0].replace(/^#\/(\d+)\/.*/, '$1');
  psql(`delete from cases where guild_id = ${cg}; insert into cases (guild_id, case_no, kind, user_id, mod_id, reason) values (${cg}, 1, 'ban', 200000000000000042, 200000000000000007, 'spam'); insert into case_proofs (guild_id, case_no, kind, body, added_by, verified, channel_id, message_id, sent_at, files) values (${cg}, 1, 'text', 'free nitro at example.test', 1, true, 300000000000000001, 5, now(), '{a.png}')`);
  await page.goto(`${base}/#/${cg}/cases`);
  await page.click('.caselist a.card', { timeout: 5000 }).catch(() => failed.push('cases: no case row'));
  await page.waitForSelector('.proofs .chip:text-is("✓ Verified")', { timeout: 5000 }).catch(() => failed.push('cases: case page shows no verified proof'));
  await page.click('.casepage button:text-is("Edit")').catch(() => failed.push('cases: no Edit button'));
  await page.fill('.casepage input[aria-label="reason"]', 'e2e reason');
  await page.selectOption('.casepage select[aria-label="case state"]', 'closed');
  await page.click('.casepage button:text-is("Save")');
  await page.waitForSelector('.edits >> text=Reason: spam → e2e reason', { timeout: 5000 }).catch(() => failed.push('cases: the edit is not in the history'));
  if (!(await page.locator('.edits >> text=State: open → closed').count())) failed.push('cases: closing is not in the history');
  await page.fill('.casepage input[aria-label="new proof"]', 'https://example.com/proof.png');
  await page.click('.casepage button.add:has-text("Add proof")');
  await page.waitForSelector('.proofs a[href="https://example.com/proof.png"]', { timeout: 5000 }).catch(() => failed.push('cases: the new proof is not listed'));
  if ((await page.inputValue('.casepage input[aria-label="new proof"]')) !== '') failed.push('cases: the proof box kept its text after a good add');
  await page.screenshot({ path: `${out}/case_page.png`, fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: `${out}/case_page_phone.png`, fullPage: true });
  if (await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)) failed.push('cases: the case page is wider than a phone');
  await page.goto(`${base}/#/${cg}/cases`);
  await page.waitForSelector('.caselist a.card', { timeout: 5000 }).catch(() => failed.push('cases: no rows on a phone'));
  await page.screenshot({ path: `${out}/case_list_phone.png`, fullPage: true });
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.screenshot({ path: `${out}/case_list.png`, fullPage: true });
  await page.click('.caselist a.card');
  await page.click('.proofs li:has(a[href="https://example.com/proof.png"]) button:text-is("Remove")', { timeout: 5000 }).catch(() => failed.push('cases: no Remove on a proof'));
  await page.click('.modal button:text-is("Remove")');
  await page.waitForSelector('.edits >> text=Removed a proof (link)', { timeout: 5000 }).catch(() => failed.push('cases: the removed proof is not in the history'));
  if (psql(`select count(*) from scheduled_jobs where guild_id = ${cg} and kind = 'case_log'`) !== '3') failed.push('cases: not one mod log entry per change');
  if (psql(`select count(*) from case_edits where guild_id = ${cg} and (old_value like '%example.com%' or new_value like '%example.com%')`) !== '0') failed.push('cases: the history kept a removed proof');
  await page.goto(`${base}/${links[0].replace(/^(#\/\d+\/).*/, '$1logging')}`);
  // No wait here on purpose: a change made right after the page loaded once got its bar hidden again (ui/PITFALLS.md #3).
  const drop = page.locator('.lp input').first();
  // Wait for the channel options too: the list has only "none" until the channel list arrives.
  await drop.click();
  await page.locator('.lpl li:nth-child(2)').first().waitFor({ timeout: 5000 }).catch(() => failed.push('cancel: no channel dropdown on logging'));
  await page.keyboard.press('Escape');
  const was = await drop.inputValue();
  await drop.click();
  const other = await page.locator('.lpl li').evaluateAll((os, w) => os.map((o) => o.textContent).find((v) => v !== w), was);
  if (other !== undefined) await page.locator('.lpl li', { hasText: other }).first().dispatchEvent('mousedown');
  await page.click('.bar >> text=Cancel', { timeout: 5000 }).catch(() => failed.push('cancel: no Changes detected bar'));
  if ((await drop.inputValue()) !== was) failed.push(`cancel: dropdown kept ${other}, saved is ${was}`);
  // Giveaways: a template is saved from the Start form (its name box opens above the form), used
  // to start a giveaway, and the giveaway is duplicated; a plan with a schedule is saved.
  psql(`update guild_config set config = config || jsonb_build_object('giveaways', jsonb_build_object('enabled', true)) where guild_id = ${gid} and version = (select max(version) from guild_config where guild_id = ${gid})`);
  await page.goto(`${base}/#/${gid}/giveaways`);
  // The Giveaways page shows the message as members see it, with the Enter button Brim adds, and an editor beside it (More tools links to the Messages page).
  await page.locator('.msgswitch .dmsg .dbtn.added', { hasText: 'Enter' }).waitFor({ timeout: 8000 }).catch(() => failed.push('giveaways: no live preview with the Enter button'));
  await page.click('.seg button:text-is("Winners post")', { timeout: 3000 }).catch(() => failed.push('giveaways: no Winners post switch'));
  await page.locator('.msgswitch .dbtn.added', { hasText: 'Claim' }).waitFor({ timeout: 8000 }).catch(() => failed.push('giveaways: the Winners post preview has no Claim button'));
  if (!(await page.locator('.msgswitch .live .ed').count()) || !(await page.locator('.msgswitch a', { hasText: 'More tools' }).count())) failed.push('giveaways: no editor or More tools link beside the preview');
  await page.reload();
  const pickGeneral = async () => { await page.getByPlaceholder('choose a channel').first().click(); await page.locator('[role=option]', { hasText: '#general' }).first().dispatchEvent('mousedown'); };
  await page.locator('button.add:has-text("Start a giveaway"):not([disabled])').first().click({ timeout: 10000 }).catch(async () => { failed.push('giveaways: Start a giveaway is off'); await page.screenshot({ path: `${out}/giveaway_off.png` }); });
  await pickGeneral();
  await page.fill('#gw-prize', 'Steam key');
  await page.fill('#gw-length', '2h');
  await page.click('button:text-is("Save as template")');
  await page.locator('.card.ask input').fill('Steam night', { timeout: 3000 }).catch(() => failed.push('giveaways: no name box for the template'));
  // The box must be on top: a click on its Save reaches it (a box under the form would not).
  await page.locator('.card.ask button:text-is("Save")').click({ timeout: 3000 }).catch(() => failed.push('giveaways: the template name box is under the form'));
  await page.waitForSelector('.rcard:has-text("Steam night"), li:has-text("Steam night")', { timeout: 5000 }).catch(() => failed.push('giveaways: saved template not listed'));
  await page.locator('button:text-is("Use")').first().click();
  if ((await page.inputValue('#gw-prize')) !== 'Steam key') failed.push('giveaways: Use did not fill the prize');
  await pickGeneral();
  await page.click('button:text-is("Start")');
  await page.waitForSelector('text=Giveaway saved', { timeout: 5000 }).catch(() => failed.push('giveaways: the start was refused'));
  await page.locator('button:text-is("Duplicate")').first().click({ timeout: 5000 }).catch(() => failed.push('giveaways: no Duplicate button'));
  if ((await page.inputValue('#gw-length').catch(() => '')) !== '2h') failed.push('giveaways: Duplicate did not copy the length');
  await page.screenshot({ path: `${out}/giveaway_duplicate.png` });
  await page.click('.modal button:text-is("Cancel")');
  psql(`delete from giveaway_plans where guild_id = ${gid}`);
  psql(`delete from giveaways where guild_id = ${gid}`);
  // Leaving a page while its requests run (or fail, as while the dashboard restarts) must not read disposed signals.
  // The two modes share no state, so each runs in its own tab at the same time.
  await Promise.all(['slow', 'fail'].map(async (mode) => {
    const tab = await ctx.newPage();
    const errs = [];
    tab.on('pageerror', (e) => errs.push(`pageerror: ${e.message}`));
    tab.on('console', (m) => m.type() === 'error' && errs.push(`console: ${m.text()}`));
    await tab.goto(`${base}/${links[0]}`);
    await tab.waitForFunction(() => document.body.innerText.trim().length >= 40, null, { timeout: 5000 }).catch(() => {});
    errs.length = 0;
    await tab.route('**/api/**', async (r) => { await new Promise((ok) => setTimeout(ok, 800)); mode === 'slow' ? r.continue() : r.abort(); });
    for (const h of links) { await tab.evaluate((x) => (location.hash = x), h); await tab.waitForTimeout(1200); }
    await tab.waitForTimeout(2000);
    await tab.unroute('**/api/**');
    const crash = errs.filter((e) => /disposed|unreachable|panicked/.test(e));
    if (crash.length) failed.push(`page switch (${mode}): ${crash[0]}`);
    await tab.close();
  }));
  await browser.close();
  if (failed.length) { console.log('FAIL\n' + failed.join('\n')); process.exit(1); }
  console.log('PASS');
})();
