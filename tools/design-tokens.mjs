// Writes crates/dashboard/ui/design/tokens.css from design/tokens.json (a copy of the Brim design system's tokens).
//   node tools/design-tokens.mjs          write tokens.css
//   node tools/design-tokens.mjs --check  exit 1 if tokens.css does not match tokens.json
// Only custom properties: no @font-face (style.css serves Poppins) and no type classes (`.small` and others are dashboard classes).
import { readFileSync, writeFileSync } from 'node:fs';

const DIR = new URL('../crates/dashboard/ui/design/', import.meta.url);
const t = JSON.parse(readFileSync(new URL('tokens.json', DIR), 'utf8'));
const theme = t.color.themes[0].id;
const val = (v) => {
  const x = typeof v === 'string' ? v : v[theme];
  const alias = /^\{([A-Za-z0-9_.-]+)\}$/.exec(x);
  return alias ? `var(--${alias[1]})` : x;
};
const lines = [
  ...t.color.tokens.map((c) => `  --${c.name}: ${val(c.value)};`),
  ...['spacing', 'radius'].flatMap((f) => t[f].tokens.map((x) => `  --${x.name}: ${x.value};`)),
  ...Object.entries(t.type.families).map(([k, v]) => `  --font-${k}: ${v};`),
  // Text sizes, one per role (page title, section title, help text ...). Style names are unique across groups.
  ...t.type.groups.flatMap((g) => g.styles.map((s) => `  --fs-${s.name}: ${s.fontSize};`)),
];
const css = `/* Brim design system tokens, generated from tokens.json by tools/design-tokens.mjs. Do not edit. */\n:root {\n${lines.join('\n')}\n}\n`;
const out = new URL('tokens.css', DIR);
if (process.argv.includes('--check')) {
  if (readFileSync(out, 'utf8') !== css) { console.error('tokens.css is stale: run node tools/design-tokens.mjs'); process.exit(1); }
} else writeFileSync(out, css);
