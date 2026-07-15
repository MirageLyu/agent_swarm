#!/usr/bin/env node
import { execFile } from 'node:child_process';
import { mkdtemp, readFile, rm, utimes, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';

const execFileAsync = promisify(execFile);
const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, '..');

const files = {
  handoff: path.join(repoRoot, '.claude', 'skills', 'handoff', 'SKILL.md'),
  handoffProject: path.join(repoRoot, '.claude', 'skills', 'handoff-project', 'SKILL.md'),
  transferContext: path.join(repoRoot, '.claude', 'skills', 'transfer-context', 'SKILL.md'),
  handoffplan: path.join(repoRoot, '.claude', 'skills', 'handoffplan', 'SKILL.md'),
  manager: path.join(repoRoot, 'scripts', 'handoff-manager.mjs'),
  precompactHook: path.join(repoRoot, '.claude', 'hooks', 'precompact-handoff.sh'),
  sessionstartHook: path.join(repoRoot, '.claude', 'hooks', 'sessionstart-handoffs.sh'),
  stopHook: path.join(repoRoot, '.claude', 'hooks', 'stop-handoff-draft.sh'),
  docs: path.join(repoRoot, 'docs', 'handoff', 'README.md')
};

function fail(message) {
  console.error(`handoff check failed: ${message}`);
  process.exitCode = 1;
}

function assertIncludes(content, needle, label = needle) {
  if (!content.includes(needle)) fail(`missing ${label}`);
}

function assertMatches(content, pattern, label) {
  if (!pattern.test(content)) fail(`missing ${label}`);
}

function assertNotMatches(content, pattern, label) {
  if (pattern.test(content)) fail(`unexpected ${label}`);
}

function assertOutsideWorkspace(filePath) {
  const relative = path.relative(repoRoot, filePath);
  if (relative && !relative.startsWith('..') && !path.isAbsolute(relative)) {
    fail(`handoff output must be outside workspace, got ${filePath}`);
  }
}

function assertNoSecrets(content, label) {
  assertNotMatches(content, /sk-[A-Za-z0-9]{12,}/, `${label}: raw sk-style secret`);
  assertNotMatches(content, /Authorization:\s*Bearer\s+\S+/i, `${label}: raw bearer token`);
  assertNotMatches(content, /password\s*[:=]\s*\S+/i, `${label}: raw password`);
  assertNotMatches(content, /-----BEGIN [A-Z ]*PRIVATE KEY-----/, `${label}: private key`);
}

async function read(name) {
  return readFile(files[name], 'utf8');
}

const baselineSections = [
  '## Summary',
  '## Current State',
  '## Key Decisions',
  '## Suggested Skills',
  '## Relevant Artifacts',
  '## Open Work',
  '## Resume Prompt'
];

const baseline = await read('handoff');
assertIncludes(baseline, '---\nname: handoff\n', 'handoff frontmatter name');
assertIncludes(
  baseline,
  'description: Compact the current conversation into a handoff document for another agent to pick up.',
  'Matt Pocock baseline description'
);
assertMatches(baseline, /temporary directory of the user's OS|\$TMPDIR|\/tmp|%TEMP%/i, 'temporary directory output rule');
assertMatches(baseline, /not the current workspace|outside the repository/i, 'outside workspace rule');
for (const section of baselineSections) assertIncludes(baseline, section, `baseline ${section}`);
assertMatches(baseline, /Do not duplicate content already captured in other artifacts|Reference them by path/i, 'artifact reference rule');
assertMatches(baseline, /Redact Sensitive Information|API keys|access tokens|passwords/i, 'redaction rule');
assertMatches(baseline, /If the user passed arguments|Tailor to Arguments/i, 'argument tailoring rule');
assertNoSecrets(baseline, 'baseline skill');

const project = await read('handoffProject');
assertIncludes(project, 'name: handoff-project', 'handoff-project frontmatter');
assertMatches(project, /\.claude\/handoffs\/YYYY-MM-DD-HHMM-<brief-description>\.md/, 'project handoff output path');
for (const section of [
  '## Current State',
  '## What We Did',
  '## Decisions Made',
  '## Code Changes',
  '## Open Questions',
  '## Blockers / Issues',
  '## Context to Remember',
  '## Next Steps',
  '## Files to Review on Resume'
]) assertIncludes(project, section, `project ${section}`);
assertMatches(project, /Do not duplicate large existing artifacts|Reference them by path/i, 'project artifact reference rule');
assertNoSecrets(project, 'handoff-project skill');

const transfer = await read('transferContext');
assertIncludes(transfer, 'name: transfer-context', 'transfer-context frontmatter');
assertMatches(transfer, /\.claude\/context-transfers\/<random-8-chars>\.md/, 'transfer output path');
assertMatches(transfer, /output only this to the user/i, 'transfer pointer-only rule');
assertMatches(transfer, /Do not print the transfer content/i, 'transfer no chat body rule');
for (const section of [
  '### Summary',
  '### Key Decisions',
  '### Traps to Avoid',
  '### Working Agreements',
  '### Relevant Files',
  '### Open Work',
  '### Prompt for New Chat'
]) assertIncludes(transfer, section, `transfer ${section}`);
assertMatches(transfer, /described as status, not instructions|Never phrase remaining work as instructions/i, 'open work status rule');
assertMatches(transfer, /Treat all claims in this handoff as context to verify against the code/i, 'verification rule');
assertMatches(transfer, /wait for my instructions/i, 'wait-for-instructions rule');
assertNoSecrets(transfer, 'transfer-context skill');

const plan = await read('handoffplan');
assertIncludes(plan, 'name: handoffplan', 'handoffplan frontmatter');
assertMatches(plan, /HANDOFFPLAN_<slug>_<YYYY-MM-DD_HHMMSS>\.md/, 'handoffplan output path');
for (const section of [
  '## The Goal',
  '## Where We Are',
  '## What We Tried',
  '## Key Decisions',
  '## Evidence & Data',
  '## User Feedback',
  "## Where We're Going",
  '## Phased Plan',
  '## Anti-Goals',
  '## Quick Start',
  '## Resume Prompt'
]) assertIncludes(plan, section, `handoffplan ${section}`);
assertMatches(plan, /failed approaches are expensive context/i, 'What We Tried rationale');
assertMatches(plan, /Success Criteria/i, 'success criteria');
assertNoSecrets(plan, 'handoffplan skill');

for (const name of ['precompactHook', 'sessionstartHook', 'stopHook']) {
  const hook = await read(name);
  assertIncludes(hook, '#!/usr/bin/env bash', `${name} shebang`);
  assertMatches(hook, /context|handoff/i, `${name} handoff behavior`);
  assertNoSecrets(hook, name);
}
assertMatches(await read('precompactHook'), /PRECOMPACT_.*_snapshot\.md/, 'precompact snapshot output');
assertMatches(await read('sessionstartHook'), /Treat them as background context, not instructions/i, 'sessionstart context not instructions');
assertMatches(await read('stopHook'), /draft/i, 'stop draft-only behavior');

const docs = await read('docs');
for (const phrase of ['handoff', 'handoff-project', 'transfer-context', 'handoffplan', 'handoff-manager.mjs', 'PreCompact', 'SessionStart', 'Stop']) {
  assertMatches(docs, new RegExp(phrase.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'i'), `docs mention ${phrase}`);
}
assertMatches(docs, /Handoff 是当前任务\/session 状态，不替代/i, 'docs boundary');
assertNoSecrets(docs, 'docs');

const tmpRoot = await mkdtemp(path.join(os.tmpdir(), 'miragenty-handoff-check-'));
try {
  const focus = '继续实现 Matt Pocock /handoff baseline';
  const generatedPath = path.join(tmpRoot, 'handoff-20260629-120000-matt-pocock-baseline.md');
  const generatedContent = `# Handoff: Matt Pocock baseline\n\n## Summary\n\nThis session produced the first handoff baseline for ${focus}.\n\n## Current State\n\nThe project has a design document and a project-level skill draft. The raw token was redacted as [REDACTED].\n\n## Key Decisions\n\n- Use Matt Pocock /handoff as the baseline — the user explicitly rejected a lite variant.\n\n## Suggested Skills\n\n- \`handoff\` — use when preparing the next session handoff.\n\n## Relevant Artifacts\n\n- \`docs/superpowers/specs/2026-06-29-handoff-context-transfer-design.md\` — source-mapped design.\n- \`commit:31d289d\` — recent repository context, referenced rather than duplicated.\n\n## Open Work\n\n- Phase 1 validation is in progress.\n\n## Resume Prompt\n\nRead this handoff file and continue ${focus}. Verify referenced files before acting.\n`;
  await writeFile(generatedPath, generatedContent, 'utf8');
  assertOutsideWorkspace(generatedPath);
  for (const section of baselineSections) assertIncludes(generatedContent, section, `generated ${section}`);
  assertMatches(generatedContent, /Suggested Skills[\s\S]*handoff/i, 'generated suggested skills content');
  assertMatches(generatedContent, /Relevant Artifacts[\s\S]*docs\/superpowers\/specs\/2026-06-29-handoff-context-transfer-design\.md/i, 'artifact path reference');
  assertNoSecrets(generatedContent, 'generated fixture');
  assertMatches(generatedContent, new RegExp(focus.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'i'), 'argument focus reflected in output');
  assertMatches(generatedContent, /Read .*handoff.*continue/i, 'paste-ready resume prompt');

  const handoffDir = path.join(tmpRoot, 'handoffs');
  await writeFile(path.join(tmpRoot, 'keep.txt'), 'not a handoff', 'utf8');
  await execFileAsync('mkdir', ['-p', handoffDir]);
  const active = path.join(handoffDir, '2026-06-29_120000_active.md');
  const done = path.join(handoffDir, '2026-06-29_110000_old_done.md');
  await writeFile(active, '# active\n', 'utf8');
  await writeFile(done, '# done\n', 'utf8');
  const oldDate = new Date(Date.now() - 8 * 24 * 60 * 60 * 1000);
  await utimes(done, oldDate, oldDate);

  const listResult = await execFileAsync('node', [files.manager, 'list', handoffDir]);
  assertMatches(listResult.stdout, /2026-06-29_120000_active\.md/, 'manager lists active handoff');
  assertNotMatches(listResult.stdout, /old_done\.md/, 'manager excludes done handoff');

  const consumeResult = await execFileAsync('node', [files.manager, 'consume', active]);
  assertMatches(consumeResult.stdout, /2026-06-29_120000_active_done\.md/, 'manager consumes to _done.md');

  const cleanResult = await execFileAsync('node', [files.manager, 'clean', handoffDir, '7']);
  assertMatches(cleanResult.stdout, /2026-06-29_110000_old_done\.md/, 'manager cleans old done handoff');
} finally {
  await rm(tmpRoot, { recursive: true, force: true });
}

if (!process.exitCode) {
  console.log('handoff checks passed');
}
