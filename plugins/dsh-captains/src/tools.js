import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const OUTSIDE = 'path is outside the Captain Drive';
const READ_CAP = 64 * 1024;
const WRITE_CAP = 256 * 1024;

export function driveDir(cfg) {
  return cfg.driveRoot;
}

const ENGINE_OWNED = new Set(['state', 'reports']);

export function isEngineOwned(first, platform = process.platform) {
  // Windows resolves `state.` (and `state `) to `state`, so a win32 segment
  // loses its trailing dots and spaces before any comparison.
  let name = platform === 'win32' ? first.replace(/[. ]+$/, '') : first;
  if (name === '' || name === '.') return false;
  if (platform === 'win32' || platform === 'darwin') name = name.toLowerCase();
  return ENGINE_OWNED.has(name);
}

export function isMergeReadyInput(parts, platform = process.platform) {
  if (!Array.isArray(parts) || parts.length !== 3) return false;
  const normalize = segment => {
    let name = platform === 'win32' ? segment.replace(/[. ]+$/, '') : segment;
    if (platform === 'win32' || platform === 'darwin') name = name.toLowerCase();
    return name;
  };
  if (normalize(parts[0]) !== 'state' || normalize(parts[1]) !== 'ready') return false;
  // The engine accepts the same bare file names it scans: alphanumerics,
  // dashes and underscores only, with no directory or extra dot.
  return /^[A-Za-z0-9_-]+\.json$/.test(parts[2]);
}

function resolveReal(target) {
  const rest = [];
  let ancestor = target;
  for (;;) {
    try {
      return { realAncestor: fs.realpathSync(ancestor), rest };
    } catch (error) {
      if (error.code !== 'ENOENT' && error.code !== 'ENOTDIR') throw error;
      // A dangling symlink names a target that cannot be verified inside.
      let link = false;
      try {
        link = fs.lstatSync(ancestor).isSymbolicLink();
      } catch {
        link = false;
      }
      if (link) throw new Error(OUTSIDE);
      const parent = path.dirname(ancestor);
      if (parent === ancestor) throw new Error(OUTSIDE);
      rest.unshift(path.basename(ancestor));
      ancestor = parent;
    }
  }
}

export function resolveInDrive(drive, rel) {
  if (typeof rel !== 'string') throw new Error(OUTSIDE);
  if (rel.includes('\0')) throw new Error(OUTSIDE);
  if (path.isAbsolute(rel)) throw new Error(OUTSIDE);
  if (rel.split(/[\\/]/).includes('..')) throw new Error(OUTSIDE);
  const root = path.resolve(drive);
  const target = path.resolve(root, rel);
  if (target !== root && !target.startsWith(root + path.sep)) throw new Error(OUTSIDE);

  // A symlinked segment carries I/O wherever the link points, so the physical
  // destination — the target's real path when it exists, or the nearest
  // existing ancestor's real path plus the segments still to be created —
  // must sit inside the Drive's real path too.
  const realDrive = fs.realpathSync(root);
  const { realAncestor, rest } = resolveReal(target);
  const realTarget = path.join(realAncestor, ...rest);
  if (realTarget !== realDrive && !realTarget.startsWith(realDrive + path.sep)) {
    throw new Error(OUTSIDE);
  }
  return { target, realTarget, realDrive };
}

let tmpCounter = 0;

function writeAtomic(realTarget, content) {
  // Resolution and I/O are not atomic: a symlink swapped between
  // resolveInDrive and this rename can still redirect the write to
  // wherever the new link points. Checking the real path narrows the
  // window; it does not close it.
  const dir = path.dirname(realTarget);
  fs.mkdirSync(dir, { recursive: true });
  const tmp = path.join(dir, `.${path.basename(realTarget)}.${process.pid}.${tmpCounter++}.tmp`);
  try {
    fs.writeFileSync(tmp, content);
    fs.renameSync(tmp, realTarget);
  } catch (error) {
    try {
      fs.unlinkSync(tmp);
    } catch {
      // the temp file never landed or the rename already moved it
    }
    throw error;
  }
}

export const DIFF_TIERS = ['docs', 'standard', 's1'];

function git(repo, args) {
  try {
    // The checkout is model-selected: never let its config run an fsmonitor
    // hook while the receipt is being read.
    return execFileSync('git', ['-C', repo, '-c', 'core.fsmonitor=false', ...args], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
  } catch (error) {
    const stderr = String(error.stderr ?? '').trim();
    throw new Error(`git ${args.join(' ')} failed${stderr === '' ? '' : `: ${stderr}`}`);
  }
}

const receiptLine = value => typeof value === 'string' && value.trim() !== '' && value.length <= 512
  && !/[\u0000-\u001f\u007f-\u009f]/.test(value);

// SUR-207: the receipt the engine's merge queue reads. HEAD and its tree are
// read from the crew checkout itself, never typed by the model, so a later
// commit — which moves both — leaves the receipt naming a commit that is no
// longer the branch tip, and the engine refuses it.
export function shipReadyReceipt(drive, args) {
  const { crewId, repo, pr, branch, slug, diffTier, gateCommand, failedOrSkipped = [], base = 'origin/main' } = args;
  if (typeof crewId !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(crewId)) {
    throw new Error('crewId must be letters, digits, dashes and underscores');
  }
  if (typeof repo !== 'string' || !path.isAbsolute(repo)) throw new Error('repo must be an absolute path');
  if (!Number.isInteger(pr) || pr <= 0) throw new Error('pr must be a pull request number');
  if (typeof slug !== 'string' || !/^[a-z0-9](?:[a-z0-9-]{0,78}[a-z0-9])?$/.test(slug)) {
    throw new Error('slug must be lowercase letters, digits and dashes');
  }
  if (!DIFF_TIERS.includes(diffTier)) throw new Error(`diffTier must be one of ${DIFF_TIERS.join(', ')}`);
  if (!Array.isArray(gateCommand) || gateCommand.length === 0 || !gateCommand.every(receiptLine)) {
    throw new Error('gateCommand must list each gate command that ran, one line each');
  }
  if (!Array.isArray(failedOrSkipped) || !failedOrSkipped.every(receiptLine)) {
    throw new Error('failedOrSkipped must name one test per entry');
  }
  if (typeof base !== 'string' || base === '' || base.startsWith('-')) throw new Error('base must name a revision');
  const current = git(repo, ['rev-parse', '--abbrev-ref', 'HEAD']);
  if (current !== branch) {
    throw new Error(`the checkout at ${repo} is on ${current}, not ${branch}; ship the branch that was reviewed`);
  }
  if (git(repo, ['status', '--porcelain', '--untracked-files=no']) !== '') {
    throw new Error(`the checkout at ${repo} has uncommitted changes; commit them before shipping`);
  }
  const receipt = {
    crewId,
    repo,
    pr,
    branch,
    headSha: git(repo, ['rev-parse', '--verify', 'HEAD^{commit}']),
    treeSha: git(repo, ['rev-parse', '--verify', 'HEAD^{tree}']),
    baseSha: git(repo, ['merge-base', 'HEAD', base]),
    gateCommand,
    diffTier,
    failedOrSkipped,
    slug,
  };
  const rel = `state/ready/${crewId}.json`;
  const { realTarget, realDrive } = resolveInDrive(drive, rel);
  // A symlinked state/ or state/ready/ must not carry the receipt into
  // another engine-owned file: the resolved destination is checked the same
  // way captain_drive_write checks it.
  if (!isMergeReadyInput(path.relative(realDrive, realTarget).split(path.sep))) {
    throw new Error(`'${rel}' does not resolve to the merge-queue receipt folder`);
  }
  writeAtomic(realTarget, JSON.stringify(receipt, null, 2) + '\n');
  return receipt;
}

export function driveToolSpecs(cfg) {
  const drive = driveDir(cfg);
  const output = {
    schema: { type: 'string' },
    render: (_args, value) => [{ type: 'text', text: value }],
  };
  return [
    {
      name: 'captain_drive_read',
      description: 'Read a file or list a directory inside the Captain Drive; entries are one per line, folders end in / and symbolic links in @',
      parameters: {
        path: { type: 'string', required: true, description: 'Path inside the Drive; "" lists the root' },
      },
      output,
      async execute({ path: rel }) {
        const { realTarget } = resolveInDrive(drive, rel);
        const stat = fs.statSync(realTarget);
        if (stat.isDirectory()) {
          return fs.readdirSync(realTarget, { withFileTypes: true })
            .filter(entry => entry.isFile() || entry.isDirectory() || entry.isSymbolicLink())
            .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))
            .map(entry => entry.name + (entry.isDirectory() ? '/' : entry.isSymbolicLink() ? '@' : ''))
            .join('\n');
        }
        if (stat.size > READ_CAP) {
          const fd = fs.openSync(realTarget, 'r');
          try {
            const body = Buffer.alloc(READ_CAP);
            const count = fs.readSync(fd, body, 0, READ_CAP, 0);
            return body.subarray(0, count).toString('utf8') + '\n[truncated]';
          } finally {
            fs.closeSync(fd);
          }
        }
        return fs.readFileSync(realTarget, 'utf8');
      },
    },
    {
      name: 'captain_drive_write',
      description: 'Write a file inside the Captain Drive',
      parameters: {
        path: { type: 'string', required: true, description: 'Path inside the Drive to write' },
        content: { type: 'string', required: true, description: 'UTF-8 text to write' },
      },
      output,
      async execute({ path: rel, content }) {
        if (process.env.DSH_PERMISSION_MODE === 'read-only') {
          throw new Error('the Captain Drive is read-only in this run');
        }
        const { realTarget, realDrive } = resolveInDrive(drive, rel);
        const relative = path.relative(realDrive, realTarget);
        const [first] = relative.split(path.sep);
        const parts = relative.split(path.sep);
        if (isEngineOwned(first) && !isMergeReadyInput(parts)) {
          throw new Error(`'${first}/' is engine-owned; the engine writes it, not the model`);
        }
        const isDirectory = realTarget === realDrive || (fs.existsSync(realTarget) && fs.statSync(realTarget).isDirectory());
        if (isDirectory) {
          throw new Error(`'${relative === '' ? '.' : relative}' is a directory; the Drive tools write files, not directories`);
        }
        const display = relative.split(path.sep).join('/');
        const bytes = Buffer.byteLength(content);
        if (bytes > WRITE_CAP) {
          throw new Error('content exceeds the 256 KB Captain Drive write limit');
        }
        writeAtomic(realTarget, content);
        return `wrote ${display} (${bytes} bytes)`;
      },
    },
    {
      name: 'captain_ship_ready',
      description: 'Mark a reviewed crew branch ready to land: reads HEAD, its tree and the merge base from the crew checkout and writes state/ready/<crewId>.json for the merge queue. Any later commit invalidates the receipt; ship again after it.',
      parameters: {
        crewId: { type: 'string', required: true, description: 'The crew chat id; names the receipt file' },
        repo: { type: 'string', required: true, description: 'Absolute path of the crew checkout, with the reviewed branch checked out' },
        pr: { type: 'integer', required: true, description: 'The pull request number' },
        branch: { type: 'string', required: true, description: 'The crew branch; must be the checked-out branch' },
        slug: { type: 'string', required: true, description: 'Names the branch\'s changelog.d/<slug>.md fragment' },
        diffTier: { type: 'string', required: true, enum: DIFF_TIERS, description: 'docs, standard or s1' },
        gateCommand: { type: 'array', required: true, items: { type: 'string' }, description: 'Each gate command that ran, e.g. cargo nextest run --locked, zgx test ui' },
        failedOrSkipped: { type: 'array', items: { type: 'string' }, description: 'Every test the gate failed or skipped; empty when all passed' },
        base: { type: 'string', description: 'Revision the merge base is taken against; defaults to origin/main' },
      },
      output,
      async execute(args) {
        if (process.env.DSH_PERMISSION_MODE === 'read-only') {
          throw new Error('the Captain Drive is read-only in this run');
        }
        const receipt = shipReadyReceipt(drive, args);
        return `wrote state/ready/${receipt.crewId}.json for ${receipt.headSha} (tree ${receipt.treeSha})`;
      },
    },
  ];
}
