// SOURCE PROPOSAL ONLY. Actions runner supplies its normal artifact runtime
// credentials to this local Node action; no user secrets or dispatch API needed.
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const cp = require('child_process');
const directory = path.resolve(process.env['INPUT_ARTIFACT-DIRECTORY']);
const uploader = path.resolve(process.env['INPUT_UPLOADER-DIRECTORY'], 'dist/upload/index.js');
const source = process.env['INPUT_SOURCE-SHA'];
if (!/^[0-9a-f]{40}$/.test(source)) throw new Error('Invalid exact source SHA');
const manifest = JSON.parse(fs.readFileSync(path.join(directory, 'piece-manifest.json'), 'utf8'));
if (manifest.schema_version !== 1 || manifest.source_sha !== source ||
    !Array.isArray(manifest.pieces) || manifest.pieces.length < 1 || manifest.pieces.length > 256) {
  throw new Error('Invalid piece manifest');
}
for (const [order, piece] of manifest.pieces.entries()) {
  if (piece.order !== order || piece.file !== `${String(order).padStart(6, '0')}.part` ||
      !Number.isSafeInteger(piece.bytes) || piece.bytes <= 0 || piece.bytes > 30 * 1024 * 1024) {
    throw new Error('Invalid bounded piece');
  }
  const member = path.join(directory, 'pieces', piece.file);
  const bytes = fs.readFileSync(member); // At most 30MiB; remote runner only.
  if (bytes.length !== piece.bytes || crypto.createHash('sha256').update(bytes).digest('hex') !== piece.sha256) {
    throw new Error('Piece digest differs');
  }
  cp.execFileSync(process.execPath, [uploader], {stdio: 'inherit', env: {
    ...process.env,
    INPUT_NAME: `native-piece-${source}-${process.env.GITHUB_RUN_ID}-${process.env.GITHUB_RUN_ATTEMPT}-${String(order).padStart(6, '0')}`,
    INPUT_PATH: member,
    'INPUT_IF-NO-FILES-FOUND': 'error',
    'INPUT_COMPRESSION-LEVEL': '0',
    'INPUT_RETENTION-DAYS': '7',
    INPUT_OVERWRITE: 'false',
    'INPUT_INCLUDE-HIDDEN-FILES': 'false',
  }});
}
