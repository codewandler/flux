import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {readdirSync, readFileSync} from 'node:fs';
import {dirname, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import test from 'node:test';

const websiteRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const repositoryRoot = resolve(websiteRoot, '..');
const cataloguePath = resolve(websiteRoot, 'docs/agent/skills-and-roles.md');
const embeddedRolesPath = 'crates/flux-agent/assets/roles';
const projectRolesPath = '.flux/agents';
const githubRoot = 'https://github.com/codewandler/flux/blob/main/';

function embeddedRoleSources() {
  return readdirSync(resolve(repositoryRoot, embeddedRolesPath))
    .filter((name) => name.endsWith('.md'))
    .map((name) => `${embeddedRolesPath}/${name}`);
}

function trackedProjectRoleSources() {
  const output = execFileSync(
    'git',
    ['ls-files', '--cached', '--', `${projectRolesPath}/*.md`],
    {cwd: repositoryRoot, encoding: 'utf8'},
  );
  return output.split('\n').filter(Boolean);
}

test('the public agent catalogue exactly matches shipped and tracked roles', () => {
  const expectedLinks = [...embeddedRoleSources(), ...trackedProjectRoleSources()]
    .map((path) => `${githubRoot}${path}`)
    .sort();
  const sourceLinkPattern =
    /https:\/\/github\.com\/codewandler\/flux\/blob\/main\/(?:crates\/flux-agent\/assets\/roles|\.flux\/agents)\/[^)\s]+\.md/g;
  const actualLinks = [...readFileSync(cataloguePath, 'utf8').matchAll(sourceLinkPattern)]
    .map(([link]) => link)
    .sort();

  assert.deepEqual(
    actualLinks,
    expectedLinks,
    'update the catalogue with exactly one canonical source link for every role',
  );
});
