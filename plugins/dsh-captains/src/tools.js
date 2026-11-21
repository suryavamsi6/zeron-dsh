import fs from 'node:fs';
import path from 'node:path';

const OUTSIDE = 'path is outside the Captain Drive';
const READ_CAP = 64 * 1024;
const WRITE_CAP = 256 * 1024;

export function driveDir(cfg) {
  return cfg.driveRoot;
}

export function resolveInDrive(drive, rel) {
  if (typeof rel !== 'string') throw new Error(OUTSIDE);
  if (rel.includes('\0')) throw new Error(OUTSIDE);
  if (path.isAbsolute(rel)) throw new Error(OUTSIDE);
  if (rel.split(/[\\/]/).includes('..')) throw new Error(OUTSIDE);
  const root = path.resolve(drive);
  const target = path.resolve(root, rel);
  if (target !== root && !target.startsWith(root + path.sep)) throw new Error(OUTSIDE);

  // A symlinked segment carries I/O wherever the link points, so the nearest
  // existing ancestor must sit inside the Drive's real path too.
  let ancestor = target;
  let realAncestor;
  for (;;) {
    try {
      realAncestor = fs.realpathSync(ancestor);
      break;
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
      ancestor = parent;
    }
  }
  const realDrive = fs.realpathSync(root);
  if (realAncestor !== realDrive && !realAncestor.startsWith(realDrive + path.sep)) {
    throw new Error(OUTSIDE);
  }
  return target;
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
      description: 'Read a file or list a directory inside the Captain Drive',
      parameters: {
        path: { type: 'string', required: true, description: 'Path inside the Drive; "" lists the root' },
      },
      output,
      async execute({ path: rel }) {
        const target = resolveInDrive(drive, rel);
        const stat = fs.statSync(target);
        if (stat.isDirectory()) {
          return fs.readdirSync(target, { withFileTypes: true })
            .filter(entry => entry.isFile() || entry.isDirectory())
            .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))
            .map(entry => entry.name + (entry.isDirectory() ? '/' : ''))
            .join('\n');
        }
        if (stat.size > READ_CAP) {
          const fd = fs.openSync(target, 'r');
          try {
            const body = Buffer.alloc(READ_CAP);
            const count = fs.readSync(fd, body, 0, READ_CAP, 0);
            return body.subarray(0, count).toString('utf8') + '\n[truncated]';
          } finally {
            fs.closeSync(fd);
          }
        }
        return fs.readFileSync(target, 'utf8');
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
        const display = typeof rel === 'string' ? rel.replace(/^(\.[\\/])+/, '') : rel;
        const [first] = typeof rel === 'string' ? display.split(/[\\/]/) : [];
        if (first === 'state' || first === 'reports') {
          throw new Error(`'${first}/' is engine-owned; the engine writes it, not the model`);
        }
        const bytes = Buffer.byteLength(content);
        if (bytes > WRITE_CAP) {
          throw new Error('content exceeds the 256 KB Captain Drive write limit');
        }
        const target = resolveInDrive(drive, rel);
        const dir = path.dirname(target);
        fs.mkdirSync(dir, { recursive: true });
        const tmp = path.join(dir, `.${path.basename(target)}.${process.pid}.${tmpCounter++}.tmp`);
        try {
          fs.writeFileSync(tmp, content);
          fs.renameSync(tmp, target);
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
  ];
}
