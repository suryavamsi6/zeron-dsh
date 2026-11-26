import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';

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
        return `wrote ${display} (${bytes} bytes)`;
      },
    },
    {
      name: 'captain_ship_ready',
      description: 'Write the merge-queue receipt for a crew branch whose pull request is ready to land; the git-derived fields are read from the worktree itself, never taken on trust',
      parameters: {
        crewId: { type: 'string', required: true, description: 'The crew chat whose branch this is; names state/ready/<crewId>.json' },
        repo: { type: 'string', required: true, description: 'Absolute path of the crew worktree the branch tip is read from' },
        pr: { type: 'number', required: true, description: 'The pull request number' },
        branch: { type: 'string', required: true, description: 'The branch the pull request is opened from' },
        reviewedSha: { type: 'string', required: true, description: 'The commit you reviewed; the write is refused when the worktree has moved past it' },
        baseSha: { type: 'string', required: true, description: 'The commit the branch built on; verified as an ancestor of the branch tip' },
        gateCommand: { type: 'string', required: true, description: 'The gate the branch survived, e.g. "cargo nextest run" or "zgx test ui"' },
        diffTier: { type: 'string', required: true, description: '"docs", "standard" or "s1"' },
        failedOrSkipped: { type: 'array', description: 'Gate legs that failed or were skipped, if any' },
        slug: { type: 'string', required: true, description: 'The changelog.d/<slug>.md fragment this branch adds' },
      },
      output,
      async execute({ crewId, repo, pr, branch, reviewedSha, baseSha, gateCommand, diffTier, failedOrSkipped, slug }) {
        if (process.env.DSH_PERMISSION_MODE === 'read-only') {
          throw new Error('the Captain Drive is read-only in this run');
        }
        // Every shape rule mirrors the engine's ReadyReceipt::validate — a
        // receipt the engine would reject is refused here, at the write.
        if (!/^[A-Za-z0-9_-]+$/.test(crewId)) throw new Error('crewId must be a bare file name');
        if (!path.isAbsolute(repo)) throw new Error('repo must be an absolute path');
        if (!Number.isInteger(pr) || pr < 1) throw new Error('pr must be a pull request number');
        // The same rule as the engine's valid_branch: alphanumerics, dots and
        // the separators, none of git's metacharacters.
        if (
          typeof branch !== 'string' || branch.length < 1 || branch.length > 200
          || branch.startsWith('-') || branch.startsWith('/') || branch.endsWith('/') || branch.endsWith('.lock')
          || branch.includes('..') || branch.includes('@{')
          || !/^[A-Za-z0-9._/-]+$/.test(branch)
        ) {
          throw new Error('branch is not a safe git branch name');
        }
        if (!/^[0-9a-f]{40}$/.test(reviewedSha)) throw new Error('reviewedSha must be a full commit SHA');
        if (!/^[0-9a-f]{40}$/.test(baseSha)) throw new Error('baseSha must be a full commit SHA');
        if (typeof gateCommand !== 'string' || gateCommand.trim() === '') {
          throw new Error('gateCommand must name the gate the branch survived');
        }
        if (!['docs', 'standard', 's1'].includes(diffTier)) throw new Error('diffTier must be "docs", "standard" or "s1"');
        if (!/^[a-z0-9-]+$/.test(slug)) throw new Error('slug must be lowercase letters, digits and dashes');
        const skipped = Array.isArray(failedOrSkipped) ? failedOrSkipped : [];
        if (skipped.some(leg => typeof leg !== 'string')) throw new Error('failedOrSkipped must be a list of gate leg names');

        // headSha and treeSha are read from the worktree, never taken as
        // parameters: the receipt can only ever name the commit that is
        // actually there, and the engine re-checks both before it merges.
        const git = args => execFileSync('git', ['-C', repo, ...args], { encoding: 'utf8' }).trim();
        let headSha, treeSha;
        try {
          headSha = git(['rev-parse', 'HEAD']);
          treeSha = git(['rev-parse', 'HEAD^{tree}']);
        } catch (error) {
          throw new Error(`could not read the worktree's tip: ${error.message}`);
        }
        // The receipt covers the reviewed commit and nothing newer: a crew
        // that pushed since the review moves the worktree's tip, and the
        // write is refused until the new tip is reviewed.
        if (headSha !== reviewedSha) {
          throw new Error(`the worktree sits on ${headSha.slice(0, 12)}, not the reviewed ${reviewedSha.slice(0, 12)} — review the new tip first`);
        }
        try {
          git(['merge-base', '--is-ancestor', baseSha, 'HEAD']);
        } catch {
          throw new Error('baseSha is not an ancestor of the branch tip — the branch was rebased since, or the base is wrong');
        }

        const receipt = {
          crewId,
          repo,
          pr,
          branch,
          headSha,
          treeSha,
          baseSha,
          gateCommand,
          diffTier,
          failedOrSkipped: skipped,
          slug,
        };
        const rel = `state/ready/${crewId}.json`;
        const { realTarget } = resolveInDrive(drive, rel);
        const dir = path.dirname(realTarget);
        fs.mkdirSync(dir, { recursive: true });
        const content = JSON.stringify(receipt, null, 2) + '\n';
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
        return `ready receipt written for ${crewId} at ${headSha.slice(0, 12)} (tree ${treeSha.slice(0, 12)})`;
      },
    },
  ];
}
