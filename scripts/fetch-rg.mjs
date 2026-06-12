import { createHash } from 'node:crypto';
import { createWriteStream } from 'node:fs';
import { chmod, copyFile, mkdir, mkdtemp, readFile, rm, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import https from 'node:https';
import { spawn } from 'node:child_process';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, '..');
const manifestPath = path.join(repoRoot, 'src-tauri', 'vendor', 'rg', 'manifest.json');

function hostTarget() {
  const archMap = {
    arm64: 'aarch64',
    x64: 'x86_64'
  };
  const platformMap = {
    darwin: 'apple-darwin',
    linux: 'unknown-linux-gnu',
    win32: 'pc-windows-msvc'
  };
  const arch = archMap[process.arch];
  const platform = platformMap[process.platform];
  if (!arch || !platform) {
    throw new Error(`Unsupported host for bundled ripgrep: ${process.platform}/${process.arch}`);
  }
  return `${arch}-${platform}`;
}

async function exists(filePath) {
  try {
    await stat(filePath);
    return true;
  } catch (error) {
    if (error && error.code === 'ENOENT') return false;
    throw error;
  }
}

const MAX_REDIRECTS = 10;

function download(url, destination, redirectCount = 0, seenUrls = new Set()) {
  if (redirectCount > MAX_REDIRECTS) {
    throw new Error(`Too many redirects while downloading ${url} (max ${MAX_REDIRECTS})`);
  }
  if (seenUrls.has(url)) {
    throw new Error(`Redirect loop detected while downloading ${url}`);
  }
  seenUrls.add(url);

  return new Promise((resolve, reject) => {
    const request = https.get(url, response => {
      if ([301, 302, 303, 307, 308].includes(response.statusCode)) {
        response.resume();
        const location = response.headers.location;
        if (!location) {
          reject(new Error(`Redirect response missing Location header for ${url}`));
          return;
        }

        let redirectUrl;
        try {
          redirectUrl = new URL(location, url);
        } catch (error) {
          reject(new Error(`Invalid redirect Location header for ${url}: ${location}`));
          return;
        }

        if (redirectUrl.protocol !== 'https:') {
          reject(new Error(`Refusing non-HTTPS redirect while downloading ${url}: ${redirectUrl.toString()}`));
          return;
        }

        Promise.resolve()
          .then(() => download(redirectUrl.toString(), destination, redirectCount + 1, seenUrls))
          .then(resolve, reject);
        return;
      }
      if (response.statusCode !== 200) {
        response.resume();
        reject(new Error(`Download failed with HTTP ${response.statusCode}: ${url}`));
        return;
      }
      const file = createWriteStream(destination);
      response.pipe(file);
      file.on('finish', () => file.close(resolve));
      file.on('error', reject);
    });
    request.on('error', reject);
  });
}

async function sha256(filePath) {
  const data = await readFile(filePath);
  return createHash('sha256').update(data).digest('hex');
}

function run(command, args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: 'inherit', ...options });
    child.on('error', reject);
    child.on('exit', code => {
      if (code === 0) resolve();
      else reject(new Error(`${command} ${args.join(' ')} exited with code ${code}`));
    });
  });
}

async function main() {
  const target = process.env.RG_TARGET || hostTarget();
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  const entry = manifest.targets[target];
  if (!entry) {
    throw new Error(`No ripgrep binary configured for target ${target}`);
  }

  const stagedPath = path.join(repoRoot, 'src-tauri', entry.resourcePath);
  if (await exists(stagedPath)) {
    console.log(`[fetch-rg] already staged: ${path.relative(repoRoot, stagedPath)}`);
    return;
  }

  const downloadsDir = path.join(repoRoot, 'src-tauri', 'vendor', 'rg', 'downloads');
  await mkdir(downloadsDir, { recursive: true });
  const archivePath = path.join(downloadsDir, entry.archive);

  if (!(await exists(archivePath))) {
    console.log(`[fetch-rg] downloading ${entry.url}`);
    await download(entry.url, archivePath);
  }

  const actualSha = await sha256(archivePath);
  if (actualSha !== entry.sha256) {
    await rm(archivePath, { force: true });
    throw new Error(`SHA-256 mismatch for ${entry.archive}: expected ${entry.sha256}, got ${actualSha}`);
  }

  const extractDir = await mkdtemp(path.join(tmpdir(), 'miragenty-rg-'));
  try {
    if (entry.archiveType !== 'tar.gz') {
      throw new Error(`Unsupported archive type: ${entry.archiveType}`);
    }
    await run('tar', ['-xzf', archivePath, '-C', extractDir]);
    const extractedBinary = path.join(extractDir, entry.executablePath);
    await mkdir(path.dirname(stagedPath), { recursive: true });
    await copyFile(extractedBinary, stagedPath);
    await chmod(stagedPath, 0o755);
    console.log(`[fetch-rg] staged ${path.relative(repoRoot, stagedPath)}`);
  } finally {
    await rm(extractDir, { recursive: true, force: true });
  }
}

main().catch(error => {
  console.error(`[fetch-rg] ${error.message}`);
  process.exit(1);
});
