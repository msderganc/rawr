// Draws the README's benchmark charts from the CSV bench.ps1 writes:
//   node scripts/charts.mjs [docs/bench.csv]
// Each chart comes in a light and a dark copy under docs/charts, for <picture>.
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';

const csvPath = process.argv[2] ?? 'docs/bench.csv';
const outDir = join(dirname(csvPath), 'charts');
mkdirSync(outDir, { recursive: true });

const THEMES = {
  light: { rawr: '#2DA44E', other: '#1E3A8A', ink: '#1F2328', muted: '#59636E', grid: '#D1D9E0' },
  dark: { rawr: '#2EA043', other: '#4C6EF5', ink: '#F0F6FC', muted: '#9198A1', grid: '#3D444D' },
};
const FONT = `-apple-system, BlinkMacSystemFont, 'Segoe UI', 'Noto Sans', Helvetica, Arial, sans-serif`;
const SETS = [
  ['many', '96 mixed files, 384 MB'],
  ['node+crates', 'Node.js install and Rust source tree, 113 MB'],
  ['node.exe', 'One executable, 89 MB'],
  ['crates', 'Rust source tree, 473 files, 11 MB'],
  ['random.bin', 'Random data, 64 MB'],
];

const [header, ...lines] = readFileSync(csvPath, 'utf8').trim().split(/\r?\n/);
const keys = header.split(',').map((k) => k.replaceAll('"', ''));
const rows = lines.map((line) => {
  const cells = line.split(',').map((c) => c.replaceAll('"', ''));
  return Object.fromEntries(keys.map((k, i) => [k, i < 2 ? cells[i] : Number(cells[i] || 0)]));
});

const escape = (s) => s.replaceAll('&', '&amp;').replaceAll('<', '&lt;');
const label = (op) => op[0].toUpperCase() + op.slice(1);

function niceStep(max, ticks = 5) {
  const raw = max / ticks;
  const power = 10 ** Math.floor(Math.log10(raw));
  return [1, 2, 2.5, 5, 10].map((m) => m * power).find((s) => s >= raw);
}

// A horizontal bar, square at the baseline and rounded 4px at the value end.
function bar(x, y, width, height, fill) {
  const r = Math.min(4, width / 2, height / 2);
  if (width <= 0) return '';
  return `<path d="M${x},${y}h${width - r}a${r},${r} 0 0 1 ${r},${r}v${height - 2 * r}a${r},${r} 0 0 1 -${r},${r}h-${width - r}z" fill="${fill}"/>`;
}

function svg(width, height, title, body) {
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}" role="img" aria-label="${escape(title)}" font-family="${FONT}">
<title>${escape(title)}</title>
${body}
</svg>
`;
}

function legend(t, x, y, otherName) {
  return `<rect x="${x}" y="${y - 9}" width="10" height="10" rx="2" fill="${t.rawr}"/>
<text x="${x + 16}" y="${y}" font-size="13" fill="${t.ink}">rawr</text>
<rect x="${x + 64}" y="${y - 9}" width="10" height="10" rx="2" fill="${t.other}"/>
<text x="${x + 80}" y="${y}" font-size="13" fill="${t.ink}">${otherName}</text>`;
}

// Seconds per operation, rawr and WinRAR as a pair of bars.
function timeChart(t, set, title) {
  const data = rows.filter((r) => r.Set === set);
  const width = 760, left = 120, right = 56, top = 64, barH = 10, gap = 2, group = 2 * barH + gap + 12;
  const plot = width - left - right;
  const max = Math.max(...data.flatMap((r) => [r.RawrSeconds, r.WinRARSeconds]));
  const step = niceStep(max);
  const axisMax = Math.ceil(max / step) * step;
  const scale = (v) => (v / axisMax) * plot;
  const height = top + data.length * group + 30;
  const parts = [
    `<text x="0" y="20" font-size="15" font-weight="600" fill="${t.ink}">${escape(title)}</text>`,
    legend(t, 0, 44, 'WinRAR 7.23'),
    `<text x="${width}" y="44" font-size="12" fill="${t.muted}" text-anchor="end">Seconds, shorter is faster</text>`,
  ];
  for (let v = 0; v <= axisMax + 1e-9; v += step) {
    const x = left + scale(v);
    parts.push(`<line x1="${x}" y1="${top - 6}" x2="${x}" y2="${height - 24}" stroke="${t.grid}" stroke-width="1"/>`);
    parts.push(`<text x="${x}" y="${height - 8}" font-size="11" fill="${t.muted}" text-anchor="middle">${+v.toFixed(2)}</text>`);
  }
  data.forEach((r, i) => {
    const y = top + i * group;
    parts.push(`<text x="${left - 10}" y="${y + barH + 4}" font-size="12" fill="${t.ink}" text-anchor="end">${escape(label(r.Operation))}</text>`);
    [[r.RawrSeconds, t.rawr, y], [r.WinRARSeconds, t.other, y + barH + gap]].forEach(([v, fill, by]) => {
      parts.push(bar(left, by, scale(v), barH, fill));
      parts.push(`<text x="${left + scale(v) + 5}" y="${by + barH - 1}" font-size="11" fill="${t.muted}">${v.toFixed(2)}</text>`);
    });
  });
  return svg(width, height, `${title}: seconds per operation, rawr and WinRAR 7.23`, parts.join('\n'));
}

// The four headline scores, against WinRAR's 100.
function scoreChart(t) {
  const geo = (xs) => 100 / Math.exp(xs.reduce((s, x) => s + Math.log(x), 0) / xs.length);
  const ratio = (a, b) => Math.max(a, 0.001) / Math.max(b, 0.001);
  const speed = geo(rows.map((r) => ratio(r.RawrSeconds, r.WinRARSeconds)));
  const memory = geo(rows.map((r) => ratio(r.RawrMB, r.WinRARMB)));
  const cpu = geo(rows.map((r) => ratio(r.RawrCpu, r.WinRARCpu)));
  const resources = Math.sqrt(memory * cpu);
  const size = geo(rows.filter((r) => r.RawrSizeMB).map((r) => r.RawrSizeMB / r.WinRARSizeMB));
  const overall = speed ** 0.7 * resources ** 0.2 * size ** 0.1;
  const scores = [
    ['Overall', overall, 'speed 70%, resources 20%, size 10%'],
    ['Speed', speed, 'wall time'],
    ['Resources', resources, `memory ${memory.toFixed(0)}, CPU time ${cpu.toFixed(0)}`],
    ['Size', size, 'archive size'],
  ];
  const width = 760, left = 100, right = 280, top = 64, barH = 18, group = 34;
  const plot = width - left - right, axisMax = 160;
  const scale = (v) => (v / axisMax) * plot;
  const height = top + scores.length * group + 28;
  const parts = [
    `<text x="0" y="20" font-size="15" font-weight="600" fill="${t.ink}">rawr's scores, where WinRAR 7.23 is 100</text>`,
    legend(t, 0, 44, 'WinRAR 7.23 = 100'),
    `<text x="${width}" y="44" font-size="12" fill="${t.muted}" text-anchor="end">Higher is better</text>`,
  ];
  for (let v = 0; v <= axisMax; v += 40) {
    const x = left + scale(v);
    parts.push(`<line x1="${x}" y1="${top - 6}" x2="${x}" y2="${height - 22}" stroke="${t.grid}" stroke-width="1"/>`);
    parts.push(`<text x="${x}" y="${height - 6}" font-size="11" fill="${t.muted}" text-anchor="middle">${v}</text>`);
  }
  scores.forEach(([name, value, note], i) => {
    const y = top + i * group;
    const weight = i === 0 ? ' font-weight="600"' : '';
    parts.push(`<text x="${left - 10}" y="${y + barH - 4}" font-size="13" fill="${t.ink}" text-anchor="end"${weight}>${name}</text>`);
    parts.push(bar(left, y, scale(value), barH, t.rawr));
    parts.push(`<text x="${left + plot + 48}" y="${y + barH - 4}" font-size="14" fill="${t.ink}" text-anchor="end"${weight}>${value.toFixed(0)}</text>`);
    parts.push(`<text x="${left + plot + 60}" y="${y + barH - 4}" font-size="12" fill="${t.muted}">${note}</text>`);
  });
  const x100 = left + scale(100);
  parts.push(`<line x1="${x100}" y1="${top - 8}" x2="${x100}" y2="${height - 22}" stroke="${t.other}" stroke-width="2" stroke-dasharray="5 3"/>`);
  return svg(width, height, `rawr's benchmark scores against WinRAR 7.23: overall ${overall.toFixed(0)}, speed ${speed.toFixed(0)}, resources ${resources.toFixed(0)}, size ${size.toFixed(0)}`, parts.join('\n'));
}

for (const [mode, t] of Object.entries(THEMES)) {
  writeFileSync(join(outDir, `scores-${mode}.svg`), scoreChart(t));
  SETS.forEach(([set, title]) => {
    const name = set.replace(/[^a-z0-9]+/g, '-');
    writeFileSync(join(outDir, `time-${name}-${mode}.svg`), timeChart(t, set, title));
  });
}
console.log(`wrote charts to ${outDir}`);
