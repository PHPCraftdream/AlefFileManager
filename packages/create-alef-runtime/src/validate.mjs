// SPDX-License-Identifier: MIT OR Apache-2.0
export function validateDirectory(value) {
  if (typeof value !== 'string' || !value || value.length > 255 || value.includes('..')
      || /[<>:"/\\|?*\p{Cc}\p{Cf}]/u.test(value) || /^[ .]|[ .]$/.test(value)
      || /^(?:con|prn|aux|nul|clock\$|conin\$|conout\$|com[1-9¹²³]|lpt[1-9¹²³])(?:\.|$)/i.test(value)) {
    throw new Error('Directory must be a Windows-portable single directory component (no paths, devices, .., controls, or leading/trailing dots/spaces).');
  }
  return value;
}

export function packageName(directory) {
  const value = directory.normalize('NFKD').replace(/\p{M}/gu, '').toLowerCase()
    .replace(/[^a-z0-9._-]+/g, '-').replace(/^[._-]+|[._-]+$/g, '');
  if (!value || value.length > 214 || !/^[a-z0-9][a-z0-9._-]*$/.test(value)
      || ['node_modules', 'favicon.ico'].includes(value)) {
    throw new Error('Directory must normalize to a valid lowercase npm package name.');
  }
  return value;
}

// Deliberate display whitelist: Unicode letters/numbers, ASCII spaces and ._-!?().
export function validateName(value) {
  if (typeof value !== 'string' || !value.trim() || !/^[\p{L}\p{N} ._!?()-]+$/u.test(value)) {
    throw new Error('Display name must contain only Unicode letters/numbers, spaces, and ._-!?() (no quotes, braces or controls).');
  }
  return value;
}

export function validateId(value) {
  // Mirrors Manifest::from_ktav_str: ASCII alphanumeric/._-, no empty dot components.
  if (typeof value !== 'string' || !/^[A-Za-z0-9_-]+(?:\.[A-Za-z0-9_-]+)*$/.test(value)) {
    throw new Error('ID must contain ASCII letters/digits, dots, underscores or hyphens, with nonempty dot-separated components.');
  }
  return value;
}

export function validateOptions(options) {
  const directory = validateDirectory(options.directory);
  const pkg = packageName(directory);
  const name = validateName(options.name ?? directory.replace(/[._-]+/g, ' ').replace(/\b\p{L}/gu, c => c.toUpperCase()));
  const id = validateId(options.id ?? `com.example.${pkg.replace(/\.+/g, '-')}`);
  const bundler = options.bundler ?? 'rsbuild';
  if (!['rsbuild', 'vite'].includes(bundler)) throw new Error('Bundler must be rsbuild or vite.');
  return { directory, name, id, bundler, package: pkg };
}
