// Fails when the dashboard leaves the shared size scale (crates/dashboard/ui/PITFALLS.md, "One set of sizes").
//   node tools/ui-sizes.mjs
// - style.css: every font-size, padding and border-radius uses a token (var(--fs-*), var(--space-*), var(--radius-*)),
//   0, a percentage or em (relative to a token). Custom property definitions (--x: 26px) are where new sizes are named.
// - style.css and design/bundle.css: every spacing (margin, padding, gap, offsets) is 4 px or a multiple of 4 (owner decision 2026-11-20), as a px value or a --space-N token. Box sizes, borders, radii and em values are not spacings.
// - style.css: font-size uses only the five text sizes plus the phone title and the strong (600) variant of ui (page-title, page-title-phone, figure, section-title, ui, caption; the style guide page #/ui lists them).
// - style.css: a page-wide max-width (800 px or more) is var(--w-wide), the one page width.
// - style.css: the screen scale --z never goes above 1.25.
// - src/*.rs: no `<div class="head">` or `class="tabs"` outside common.rs (Head and PageTabs are the only header and tab row); style.css: the .tabs rules keep the orange underline on the open tab and no filled pill (tabs are fixed by the owner 2026-11-20).
// - src/*.rs: no fixed style="..." attribute; a page uses a class. A computed style (a colour, a position) is fine.
// - src/*.rs: no button whose text starts with "+" and no `label="+..."` outside common.rs (AddButton is the one add button; the number steppers' "+" with aria-label="more" are controls, not add buttons); no dashed wide button.
// - src/*.rs: every <table> has class "data" (the table card: one header style, labelled rows on a phone; owner decision 2026-11-20).
// - style.css: `.cards` is the two-column flow (Cards in common.rs), never a CSS grid again (owner decision 2026-11-20 "no empty spaces").
// - src/*.rs: no <datalist> or list= input, and no <select> for a channel, role or category: ListPick in form.rs is the one picker (ui PITFALLS "One picker"; only the time zone box in msgrules.rs may keep its list).
import { readFileSync, readdirSync } from 'node:fs';

const UI = new URL('../crates/dashboard/ui/', import.meta.url);
const bad = [];
// Channel Setup's channel list is compact on purpose and is free of the 4 px scale and the padding tokens
// (owner decision 2026-11-21, ui PITFALLS "Channel Setup list is compact"): only its row rules.
const CS_LIST = /^\.cs \.(tr|slot|nm)\b/;
// Comments are blanked (newlines kept, so line numbers stay right).
const css = readFileSync(new URL('style.css', UI), 'utf8').replace(/\/\*[\s\S]*?\*\//g, (c) => c.replace(/[^\n]/g, ' '));
css.split('\n').forEach((line, i) => {
  if (CS_LIST.test(line)) return;
  for (const m of line.matchAll(/(?:^|[;{\s])(font|font-size|padding[a-z-]*|border[a-z-]*radius)\s*:([^;}]*)/g)) {
    const raw = m[2].replace(/var\([^)]*\)/g, '');
    if (/\d(px|rem|pt|vh|vw|vmin|vmax|dvh|ch)\b/.test(raw)) bad.push(`style.css:${i + 1}: ${m[1]}:${m[2].trim()} is not a token`);
  }
});
const SPACING = /(?:^|[;{\s])((?:margin|padding|gap|row-gap|column-gap|top|left|right|bottom|inset|scroll-margin|scroll-padding)[a-z-]*)\s*:([^;}]*)/g;
for (const file of ['style.css', 'design/bundle.css']) {
  readFileSync(new URL(file, UI), 'utf8').replace(/\/\*[\s\S]*?\*\//g, (c) => c.replace(/[^\n]/g, ' ')).split('\n').forEach((line, i) => {
    if (file === 'style.css' && CS_LIST.test(line)) return;
    for (const m of line.matchAll(SPACING)) {
      for (const n of m[2].matchAll(/(?<![\w.-])(-?\d+(?:\.\d+)?)px|--space-(\d+)/g)) {
        const v = Number(n[1] ?? n[2]);
        if (v % 4) bad.push(`${file}:${i + 1}: ${m[1]}:${m[2].trim()} is not 4 px or a multiple of 4`);
      }
    }
  });
}
// Screen scale: one `zoom` (the :root rule) and no bare vw/vh, which the browser does not zoom (design/README.md, "Screen scale").
css.split('\n').forEach((line, i) => {
  if (/(^|[;{\s])zoom\s*:/.test(line) && !/--z\b/.test(line)) bad.push(`style.css:${i + 1}: zoom is set only on :root from var(--z)`);
  if (/(?<![\w.-])\d+(?:\.\d+)?(vh|vw)\b/.test(line.replace(/--v[wh]:[^;]*;/g, ''))) bad.push(`style.css:${i + 1}: a bare vw/vh is not zoomed; use calc(N * var(--vw)) or var(--vh)`);
});
const SIZES = new Set(['page-title', 'page-title-phone', 'figure', 'section-title', 'ui', 'ui-strong', 'caption']);
css.split('\n').forEach((line, i) => {
  for (const m of line.matchAll(/(?:^|[;{\s])font(?:-size)?\s*:[^;}]*?var\(--fs-([a-z-]+)[,)]/g)) {
    if (!SIZES.has(m[1])) bad.push(`style.css:${i + 1}: font-size var(--fs-${m[1]}) is not one of the five text sizes`);
  }
});
// A page-wide max-width (800 px or more) is var(--w-wide); smaller ones (dialogs, previews) are free.
css.split('\n').forEach((line, i) => {
  for (const m of line.matchAll(/(?<![-\w(])max-width\s*:\s*(\d+)px/g)) {
    if (+m[1] >= 800) bad.push(`style.css:${i + 1}: max-width ${m[1]}px, use var(--w-wide)`);
  }
});
// The screen scale stops at 1.25 (from 1920 px up): a full-page zoom above that made 2560 px look zoomed in (owner decision 2026-11-21).
for (const m of css.matchAll(/--z\s*:\s*([\d.]+)/g)) if (+m[1] > 1.25) bad.push(`style.css: --z:${m[1]} is above 1.25; the scale stops at 1920 px (ui PITFALLS, "Screen scale stops at 1920")`);
// Every rule (inside media queries too) whose selector's last part is .cards itself, not something inside it.
for (const m of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
  if (m[1].split(',').some((sel) => /\.cards(?![\w-])[^\s>+~]*$/.test(sel.trim())) && /display:\s*grid|grid-template/.test(m[2])) bad.push(`style.css: "${m[1].trim()}" makes .cards a grid; it is the two-column flow (Cards in common.rs), so cards move up with no holes`);
}
const tabRules = css.split('\n').flatMap((l) => [...l.matchAll(/(\.tabs[^{}]*)\{([^}]*)\}/g)]);
if (!tabRules.some((m) => /\.tabs [^{]*\.on\b/.test(m[1]) && /border-bottom-color:\s*var\(--primary\)/.test(m[2]))) bad.push('style.css: the open .tabs tab lost its orange underline (border-bottom-color:var(--primary)); tabs are fixed by the owner');
for (const m of tabRules) if (/background:\s*var\(--(primary|b2)\)/.test(m[2]) || /radius-card/.test(m[2])) bad.push(`style.css: .tabs rule "${m[1].trim()}" has a filled or boxed look; tabs are an underline row`);
for (const f of readdirSync(new URL('src/', UI))) {
  if (!f.endsWith('.rs')) continue;
  readFileSync(new URL(`src/${f}`, UI), 'utf8').split('\n').forEach((line, i) => {
    if (f !== 'common.rs' && /class="(head|tabs)"/.test(line)) bad.push(`src/${f}:${i + 1}: use Head / PageTabs from common.rs, not a hand-written head or tab row`);
    if (/\bstyle=["']/.test(line)) bad.push(`src/${f}:${i + 1}: fixed style="..." attribute, use a class`);
    if (f !== 'common.rs' && !/aria-label="more"/.test(line) && (/>"\+[^"]*"<\/button>/.test(line) || /\blabel="\+/.test(line))) bad.push(`src/${f}:${i + 1}: a "+" button, use AddButton from common.rs`);
    if (/<table\b/.test(line) && !/<table class="(?:[^"]*\s)?data(?=[\s"])/.test(line)) bad.push(`src/${f}:${i + 1}: a table without class "data", use the table card (table.data inside a card)`);
    if (f !== 'msgrules.rs' && /<datalist\b|<input[^>]*\slist=/.test(line)) bad.push(`src/${f}:${i + 1}: a datalist input, use ListPick from form.rs (the one picker)`);
    if (/<select\b[^>]*(channel|role|categor)/i.test(line) && !/aria-label="Raider List category"|CATEGORIES/.test(line)) bad.push(`src/${f}:${i + 1}: a select for a channel, role or category, use ListPick from form.rs (the one picker)`);
    if (/class="[^"]*\bdashed\b/.test(line)) bad.push(`src/${f}:${i + 1}: dashed wide button, use AddButton from common.rs`);
  });
}
if (bad.length) {
  console.error(bad.join('\n'));
  console.error('Use the size tokens (design/tokens.css, the --fs-* line at the top of style.css) and classes.');
  process.exit(1);
}
