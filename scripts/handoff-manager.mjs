#!/usr/bin/env node
import { readdir, rename, rm, stat } from 'node:fs/promises';
import path from 'node:path';

function usage() {
  console.error(`Usage:
  node scripts/handoff-manager.mjs list [dir]
  node scripts/handoff-manager.mjs consume <file>
  node scripts/handoff-manager.mjs clean [dir] [days]

Defaults:
  dir  = .claude/handoffs
  days = 7
`);
}

function repoDefaultDir() {
  return path.resolve(process.cwd(), '.claude', 'handoffs');
}

function isDone(fileName) {
  return fileName.endsWith('_done.md');
}

function isMarkdown(fileName) {
  return fileName.endsWith('.md');
}

async function listActive(dir) {
  const root = path.resolve(dir || repoDefaultDir());
  let entries = [];
  try {
    entries = await readdir(root, { withFileTypes: true });
  } catch (error) {
    if (error && error.code === 'ENOENT') return [];
    throw error;
  }
  return entries
    .filter((entry) => entry.isFile())
    .map((entry) => entry.name)
    .filter((name) => isMarkdown(name) && !isDone(name))
    .sort()
    .map((name) => path.join(root, name));
}

function donePath(filePath) {
  if (!filePath.endsWith('.md')) {
    throw new Error(`handoff file must end with .md: ${filePath}`);
  }
  if (filePath.endsWith('_done.md')) {
    return filePath;
  }
  return filePath.slice(0, -'.md'.length) + '_done.md';
}

async function consume(filePath) {
  const source = path.resolve(filePath);
  const destination = donePath(source);
  if (source === destination) {
    return destination;
  }
  await rename(source, destination);
  return destination;
}

async function clean(dir, days = 7) {
  const root = path.resolve(dir || repoDefaultDir());
  const thresholdMs = Number(days) * 24 * 60 * 60 * 1000;
  const now = Date.now();
  let entries = [];
  try {
    entries = await readdir(root, { withFileTypes: true });
  } catch (error) {
    if (error && error.code === 'ENOENT') return [];
    throw error;
  }

  const removed = [];
  for (const entry of entries) {
    if (!entry.isFile() || !isDone(entry.name)) continue;
    const filePath = path.join(root, entry.name);
    const info = await stat(filePath);
    if (now - info.mtimeMs > thresholdMs) {
      await rm(filePath, { force: true });
      removed.push(filePath);
    }
  }
  return removed.sort();
}

const [command, first, second] = process.argv.slice(2);

try {
  if (command === 'list') {
    const active = await listActive(first);
    console.log(active.join('\n'));
  } else if (command === 'consume') {
    if (!first) {
      usage();
      process.exitCode = 2;
    } else {
      console.log(await consume(first));
    }
  } else if (command === 'clean') {
    const removed = await clean(first, second ?? 7);
    console.log(removed.join('\n'));
  } else {
    usage();
    process.exitCode = 2;
  }
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
}
