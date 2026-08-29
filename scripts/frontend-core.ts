#!/usr/bin/env bun

import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(scriptDir, '..');
const templateDir = path.join(repoRoot, 'template_frontend');
const studioDir = path.join(repoRoot, 'sgs_frontend');

/**
 * Canonical shared game-frontend core.
 *
 * `template_frontend/` is the single hand-maintained source of these files.
 * `sync:core` copies them into `sgs_frontend/` and `check:core` (CI) fails if
 * the copies drift. Files that are intentionally per-frontend are NOT listed
 * here: `index.html`, `package.json`, `src/App.tsx`, `src/index.css`,
 * `src/components/Layout.*`, `src/games/number-guess/NumberGuessGame.tsx`.
 */
const CORE_FILES = [
  '.npmrc',
  'postcss.config.js',
  'tailwind.config.js',
  'tsconfig.json',
  'tsconfig.node.json',
  'vite.config.ts',
  'src/main.tsx',
  'src/config.ts',
  'src/components/LayoutStandalone.css',
  'src/components/LayoutStandalone.tsx',
  'src/components/WalletStandalone.css',
  'src/components/WalletStandalone.tsx',
  'src/components/WalletSwitcher.css',
  'src/components/WalletSwitcher.tsx',
  'src/games/number-guess/bindings.ts',
  'src/games/number-guess/numberGuessService.ts',
  'src/hooks/useWallet.ts',
  'src/hooks/useWalletStandalone.ts',
  'src/services/devWalletService.ts',
  'src/store/walletSlice.ts',
  'src/types/signer.ts',
  'src/utils/authEntryUtils.ts',
  'src/utils/constants.ts',
  'src/utils/ledgerUtils.ts',
  'src/utils/requestCache.ts',
  'src/utils/runtimeConfig.ts',
  'src/utils/simulationUtils.ts',
  'src/utils/transactionHelper.ts',
];

function readText(file: string): string {
  return readFileSync(file, 'utf8');
}

function missing(reason: string, rel: string): never {
  console.error(`❌ ${reason}: ${rel}`);
  process.exit(1);
}

function sync() {
  let changed = 0;
  for (const rel of CORE_FILES) {
    const src = path.join(templateDir, rel);
    const dest = path.join(studioDir, rel);
    if (!existsSync(src)) missing(`Missing template file`, rel);

    const contents = readText(src);
    if (!existsSync(dest) || readText(dest) !== contents) {
      mkdirSync(path.dirname(dest), { recursive: true });
      writeFileSync(dest, contents);
      changed++;
    }
  }
  console.log(
    changed
      ? `✅ Synced ${changed} shared core file(s) from template_frontend/ to sgs_frontend/`
      : '✅ Shared core already in sync'
  );
}

function check() {
  const drifted: string[] = [];
  const missingFiles: string[] = [];
  for (const rel of CORE_FILES) {
    const src = path.join(templateDir, rel);
    const dest = path.join(studioDir, rel);
    if (!existsSync(src)) missing(`Missing template file`, rel);
    if (!existsSync(dest)) {
      missingFiles.push(`sgs_frontend/${rel}`);
      continue;
    }
    if (readText(src) !== readText(dest)) drifted.push(rel);
  }

  for (const rel of missingFiles) {
    console.error(`   - ${rel} (missing in sgs_frontend/)`);
  }
  if (drifted.length || missingFiles.length) {
    console.error('❌ Shared frontend core has drifted from template_frontend/:');
    for (const rel of drifted) {
      console.error(`   - ${rel}`);
    }
    console.error('   Run "bun run sync:core" to copy template_frontend/ into sgs_frontend/.');
    process.exit(1);
  }
  console.log('✅ Shared frontend core matches template_frontend/');
}

const syncFlag = process.argv.includes('--sync');

if (syncFlag) {
  sync();
} else {
  check();
}