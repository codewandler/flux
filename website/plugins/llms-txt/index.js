// A build-time plugin that emits /llms.txt and /llms-full.txt from the docs
// source, following the https://llmstxt.org convention. Generated on every
// `docusaurus build`, so the files can never drift from the published pages.
//
// - llms.txt      — a curated index: title, summary, then one link list per
//                   sidebar category, each link annotated with a one-line blurb.
// - llms-full.txt — the entire docs corpus inlined, in sidebar order, so a model
//                   can ingest the whole documentation set in one fetch.

const fs = require('fs');
const path = require('path');

const DOCS_DIR = path.join(__dirname, '..', '..', 'docs');
const sidebars = require('../../sidebars.js');

/** Read a doc file by its sidebar id (e.g. "agent/a2a"), trying `.md`/`.mdx`. */
function readDoc(id) {
  for (const ext of ['.md', '.mdx']) {
    const file = path.join(DOCS_DIR, id + ext);
    if (fs.existsSync(file)) {
      return {id, file, raw: fs.readFileSync(file, 'utf8')};
    }
  }
  return null;
}

/** Split leading `---` YAML frontmatter from the markdown body. */
function splitFrontmatter(raw) {
  const m = raw.match(/^---\n([\s\S]*?)\n---\n?/);
  if (!m) return {front: {}, body: raw};
  const front = {};
  for (const line of m[1].split('\n')) {
    const kv = line.match(/^([A-Za-z0-9_-]+):\s*(.*)$/);
    if (kv) front[kv[1]] = kv[2].replace(/^['"]|['"]$/g, '').trim();
  }
  return {front, body: raw.slice(m[0].length)};
}

/** A page's display title: frontmatter title → first H1 → humanized id. */
function titleOf(id, front, body) {
  if (front.title) return front.title;
  const h1 = body.match(/^#\s+(.+)$/m);
  if (h1) return h1[1].trim();
  return id.split('/').pop().replace(/[-_]/g, ' ');
}

/** First real prose paragraph, condensed to a single line for the index blurb. */
function blurbOf(body) {
  const stripped = body
    .replace(/^#.*$/gm, '') // headings
    .replace(/^import\s.+$/gm, '') // stray MDX imports
    .replace(/:::[\s\S]*?:::/g, ''); // admonitions
  for (const block of stripped.split(/\n\s*\n/)) {
    const line = block.trim();
    if (!line) continue;
    if (line.startsWith('```') || line.startsWith('|') || line.startsWith('<')) continue;
    if (line.startsWith('-') || line.startsWith('*')) continue;
    return line.replace(/\s+/g, ' ').replace(/`/g, '').replace(/\[([^\]]+)\]\([^)]+\)/g, '$1');
  }
  return '';
}

/**
 * Walk the sidebar into ordered sections: a leading "Overview" section for the
 * top-level pages, then one section per top-level category (nested categories
 * are flattened into their parent, preserving order).
 */
function sectionsFromSidebar() {
  const items = sidebars.docs || [];
  const sections = [];
  let overview = {label: 'Overview', ids: []};

  const collectIds = (node, acc) => {
    if (typeof node === 'string') acc.push(node);
    else if (node && node.type === 'category') for (const c of node.items) collectIds(c, acc);
    else if (node && node.id) acc.push(node.id);
  };

  for (const node of items) {
    if (typeof node === 'string') {
      overview.ids.push(node);
    } else if (node && node.type === 'category') {
      const ids = [];
      for (const c of node.items) collectIds(c, ids);
      sections.push({label: node.label, ids});
    }
  }
  if (overview.ids.length) sections.unshift(overview);
  return sections;
}

/** @returns {import('@docusaurus/types').Plugin} */
module.exports = function llmsTxtPlugin() {
  return {
    name: 'llms-txt',
    async postBuild({siteConfig, outDir}) {
      const base = siteConfig.url + siteConfig.baseUrl.replace(/\/$/, ''); // e.g. https://…/flux
      const docUrl = (id) => `${base}/docs/${id}`;
      const sections = sectionsFromSidebar();

      // ---- llms.txt (curated index) ----
      const idx = [];
      idx.push(`# ${siteConfig.title}`, '');
      idx.push(`> ${siteConfig.tagline}`, '');
      idx.push(
        'flux compiles each request into a typed Flux-Lang plan (a small graph) that a deterministic',
        'Rust runtime executes through one mandatory safety envelope (authorization → approval →',
        'guarded IO). The documentation below covers the agent, the language, the SDK, plugins, and',
        'operations.',
        '',
      );
      idx.push(`Full documentation as a single file: [llms-full.txt](${base}/llms-full.txt)`, '');

      for (const section of sections) {
        const links = [];
        for (const id of section.ids) {
          const doc = readDoc(id);
          if (!doc) continue;
          const {front, body} = splitFrontmatter(doc.raw);
          const title = titleOf(id, front, body);
          const blurb = blurbOf(body);
          links.push(`- [${title}](${docUrl(id)})${blurb ? ': ' + blurb : ''}`);
        }
        if (!links.length) continue;
        idx.push(`## ${section.label}`, '', ...links, '');
      }

      fs.writeFileSync(path.join(outDir, 'llms.txt'), idx.join('\n').replace(/\n{3,}/g, '\n\n'));

      // ---- llms-full.txt (entire corpus inlined) ----
      const full = [];
      full.push(`# ${siteConfig.title} — full documentation`, '');
      full.push(`> ${siteConfig.tagline}`, '');
      full.push(`Source: ${base}/`, '');
      for (const section of sections) {
        for (const id of section.ids) {
          const doc = readDoc(id);
          if (!doc) continue;
          const {front, body} = splitFrontmatter(doc.raw);
          const title = titleOf(id, front, body);
          full.push('', '---', '', `# ${title}`, `URL: ${docUrl(id)}`, '', body.trim(), '');
        }
      }
      fs.writeFileSync(path.join(outDir, 'llms-full.txt'), full.join('\n'));

      console.log('[llms-txt] wrote llms.txt and llms-full.txt to', outDir);
    },
  };
};
